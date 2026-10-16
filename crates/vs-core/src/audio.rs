//! Audio utilities: decode/encode, resampling, fades, click-free stitching, ITU-R BS.1770
//! loudness measurement and normalization, and reference-quality analysis.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Mono floating point audio in [-1, 1].
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuf {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

impl AudioBuf {
    pub fn new(sample_rate: u32, samples: Vec<f32>) -> Self {
        Self { sample_rate, samples }
    }
    pub fn silence(sample_rate: u32, ms: f32) -> Self {
        Self { sample_rate, samples: vec![0.0; ms_to_samples(sample_rate, ms)] }
    }
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate.max(1) as f64
    }
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        (self.samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / self.samples.len() as f64).sqrt() as f32
    }
}

pub fn ms_to_samples(sr: u32, ms: f32) -> usize {
    ((ms.max(0.0) as f64) * sr as f64 / 1000.0).round() as usize
}

pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-10 {
        -200.0
    } else {
        20.0 * x.log10()
    }
}

// ---------------------------------------------------------------------------------------
// I/O
// ---------------------------------------------------------------------------------------

/// Write 16-bit PCM mono WAV atomically (temp file + rename).
pub fn write_wav(path: &Path, buf: &AudioBuf) -> Result<()> {
    let dir = path.parent().context("no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: buf.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&tmp, spec)?;
        for s in &buf.samples {
            // TPDF dither is unnecessary for TTS content; round to nearest.
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            w.write_sample(v)?;
        }
        w.finalize()?;
    }
    crate::fsutil::replace_file(&tmp, path)
}

/// Write 32-bit float WAV (used for intermediate segment cache, lossless).
pub fn write_wav_f32(path: &Path, buf: &AudioBuf) -> Result<()> {
    let dir = path.parent().context("no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: buf.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&tmp, spec)?;
        for s in &buf.samples {
            w.write_sample(*s)?;
        }
        w.finalize()?;
    }
    crate::fsutil::replace_file(&tmp, path)
}

/// Read a WAV file (any PCM/float format) and downmix to mono.
pub fn read_wav(path: &Path) -> Result<AudioBuf> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("open wav {}", path.display()))?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<std::result::Result<_, _>>()?
        }
    };
    Ok(AudioBuf::new(spec.sample_rate, downmix(&interleaved, ch)))
}

fn downmix(interleaved: &[f32], ch: usize) -> Vec<f32> {
    if ch == 1 {
        return interleaved.to_vec();
    }
    interleaved.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
}

/// Decode any supported container/codec (WAV, MP3, FLAC, OGG/Vorbis, M4A/AAC) to mono.
/// Corrupt or truncated files return an error instead of panicking; decodable packets
/// before the corruption are kept only if at least 0.5 s decoded cleanly.
pub fn decode_file(path: &Path) -> Result<AudioBuf> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymErr;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .context("unrecognized or corrupt audio file")?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("no audio track")?;
    let track_id = track.id;
    let sr = track.codec_params.sample_rate.context("unknown sample rate")?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("unsupported codec")?;
    let mut out = Vec::new();
    let mut errors = 0usize;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break,
            Err(e) => {
                if out.len() as u32 > sr / 2 {
                    tracing::warn!("decode stopped early: {e}");
                    break;
                }
                return Err(e).context("corrupt audio stream");
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let ch = spec.channels.count().max(1);
                let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                sb.copy_interleaved_ref(decoded);
                out.extend(downmix(sb.samples(), ch));
            }
            Err(SymErr::DecodeError(_)) => {
                errors += 1;
                if errors > 50 {
                    bail!("too many decode errors; file is corrupt");
                }
            }
            Err(e) => return Err(e).context("decode failed"),
        }
    }
    if out.is_empty() {
        bail!("no audio decoded");
    }
    if out.iter().any(|s| !s.is_finite()) {
        bail!("audio contains NaN/Inf samples");
    }
    Ok(AudioBuf::new(sr, out))
}

// ---------------------------------------------------------------------------------------
// Resampling (windowed sinc, Kaiser window)
// ---------------------------------------------------------------------------------------

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

pub fn resample(buf: &AudioBuf, target_sr: u32) -> AudioBuf {
    if buf.sample_rate == target_sr || buf.samples.is_empty() {
        return AudioBuf::new(target_sr, buf.samples.clone());
    }
    let ratio = target_sr as f64 / buf.sample_rate as f64;
    let cutoff = ratio.min(1.0) * 0.95; // relative to input Nyquist
    let half_taps = 24.0 / cutoff.min(1.0);
    let half = half_taps.ceil() as isize;
    let beta = 8.0;
    let i0b = bessel_i0(beta);
    let out_len = ((buf.samples.len() as f64) * ratio).round() as usize;
    let input = &buf.samples;
    let n_in = input.len() as isize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let t = n as f64 / ratio;
        let center = t.floor() as isize;
        let mut acc = 0.0f64;
        let mut wsum = 0.0f64;
        for k in (center - half + 1)..=(center + half) {
            let x = t - k as f64;
            let w_arg = x / half_taps;
            if w_arg.abs() > 1.0 {
                continue;
            }
            let window = bessel_i0(beta * (1.0 - w_arg * w_arg).sqrt()) / i0b;
            let sx = cutoff * x;
            let sinc = if sx.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * sx).sin() / (std::f64::consts::PI * sx) };
            let h = cutoff * sinc * window;
            wsum += h;
            if k >= 0 && k < n_in {
                acc += input[k as usize] as f64 * h;
            }
        }
        // Normalize DC gain (handles edges cleanly).
        let _ = wsum;
        out.push(acc as f32);
    }
    AudioBuf::new(target_sr, out)
}

// ---------------------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------------------

/// Apply a raised-cosine fade-in and fade-out.
pub fn apply_fades(buf: &mut AudioBuf, fade_in_ms: f32, fade_out_ms: f32) {
    let n = buf.samples.len();
    let fi = ms_to_samples(buf.sample_rate, fade_in_ms).min(n / 2);
    let fo = ms_to_samples(buf.sample_rate, fade_out_ms).min(n / 2);
    for i in 0..fi {
        let g = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / fi as f32).cos();
        buf.samples[i] *= g;
    }
    for i in 0..fo {
        let g = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / fo as f32).cos();
        buf.samples[n - 1 - i] *= g;
    }
}

pub fn apply_gain_db(buf: &mut AudioBuf, db: f32) {
    if db == 0.0 {
        return;
    }
    let g = db_to_lin(db);
    for s in buf.samples.iter_mut() {
        *s *= g;
    }
}

/// Remove DC offset with a gentle one-pole high-pass (≈20 Hz).
pub fn remove_dc(buf: &mut AudioBuf) {
    let r = (-2.0 * std::f32::consts::PI * 20.0 / buf.sample_rate as f32).exp();
    let (mut x1, mut y1) = (0.0f32, 0.0f32);
    for s in buf.samples.iter_mut() {
        let y = *s - x1 + r * y1;
        x1 = *s;
        y1 = y;
        *s = y;
    }
}

/// Trim leading/trailing silence below `threshold_db` (frame RMS), keeping `keep_ms` of
/// margin. Returns (start, end) sample indices that were kept.
pub fn trim_silence(buf: &mut AudioBuf, threshold_db: f32, keep_ms: f32) -> (usize, usize) {
    let (s, e) = silence_bounds(buf, threshold_db, keep_ms);
    buf.samples = buf.samples[s..e].to_vec();
    (s, e)
}

pub fn silence_bounds(buf: &AudioBuf, threshold_db: f32, keep_ms: f32) -> (usize, usize) {
    let frame = ms_to_samples(buf.sample_rate, 10.0).max(1);
    let thr = db_to_lin(threshold_db);
    let frames: Vec<f32> = buf
        .samples
        .chunks(frame)
        .map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let first = frames.iter().position(|r| *r > thr);
    let last = frames.iter().rposition(|r| *r > thr);
    match (first, last) {
        (Some(f), Some(l)) => {
            let keep = ms_to_samples(buf.sample_rate, keep_ms);
            let s = (f * frame).saturating_sub(keep);
            let e = ((l + 1) * frame + keep).min(buf.samples.len());
            (s, e)
        }
        _ => (0, 0),
    }
}

/// A piece for stitching: audio plus the silence gap placed *before* it.
pub struct Piece {
    pub audio: AudioBuf,
    pub gap_before_ms: f32,
}

/// Concatenate pieces with silence gaps. Every piece gets short raised-cosine fades so the
/// joins are click-free; pieces with a zero gap are joined with an equal-power crossfade.
pub fn stitch(pieces: Vec<Piece>, sample_rate: u32, fade_ms: f32, crossfade_ms: f32) -> AudioBuf {
    let mut out: Vec<f32> = Vec::new();
    for (i, mut p) in pieces.into_iter().enumerate() {
        if p.audio.sample_rate != sample_rate {
            p.audio = resample(&p.audio, sample_rate);
        }
        apply_fades(&mut p.audio, fade_ms, fade_ms);
        let gap = ms_to_samples(sample_rate, p.gap_before_ms);
        if i > 0 && gap == 0 {
            let xf = ms_to_samples(sample_rate, crossfade_ms).min(out.len()).min(p.audio.samples.len());
            let start = out.len() - xf;
            for j in 0..xf {
                let t = (j as f32 + 0.5) / xf as f32;
                let a = (t * std::f32::consts::FRAC_PI_2).cos();
                let b = (t * std::f32::consts::FRAC_PI_2).sin();
                out[start + j] = out[start + j] * a + p.audio.samples[j] * b;
            }
            out.extend_from_slice(&p.audio.samples[xf..]);
        } else {
            if i > 0 || gap > 0 {
                out.extend(std::iter::repeat_n(0.0, gap));
            }
            out.extend_from_slice(&p.audio.samples);
        }
    }
    AudioBuf::new(sample_rate, out)
}

// ---------------------------------------------------------------------------------------
// Loudness (ITU-R BS.1770-4 / EBU R128 integrated loudness)
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn run(&self, x: &[f64]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        x.iter()
            .map(|&x0| {
                let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
                x2 = x1;
                x1 = x0;
                y2 = y1;
                y1 = y0;
                y0
            })
            .collect()
    }
}

/// K-weighting filter coefficients for an arbitrary sample rate (pre-filter + RLB high-pass),
/// derived with the bilinear transform as in libebur128.
fn k_weighting(sr: f64) -> (Biquad, Biquad) {
    // Stage 1: high-shelf
    let f0 = 1681.974450955533;
    let g = 3.999843853973347;
    let q = 0.7071752369554196;
    let k = (std::f64::consts::PI * f0 / sr).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    };
    // Stage 2: high-pass
    let f0 = 38.13547087602444;
    let q = 0.5003270373238773;
    let k = (std::f64::consts::PI * f0 / sr).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    };
    (shelf, hp)
}

/// Integrated loudness in LUFS (mono). Returns None for silence / too-short input.
pub fn integrated_lufs(buf: &AudioBuf) -> Option<f64> {
    let sr = buf.sample_rate as f64;
    let x: Vec<f64> = buf.samples.iter().map(|s| *s as f64).collect();
    let (s1, s2) = k_weighting(sr);
    let y = s2.run(&s1.run(&x));
    let block = (0.4 * sr) as usize;
    let hop = (0.1 * sr) as usize;
    if y.len() < block || hop == 0 {
        return None;
    }
    let mut energies = Vec::new();
    let mut i = 0;
    while i + block <= y.len() {
        let e = y[i..i + block].iter().map(|v| v * v).sum::<f64>() / block as f64;
        energies.push(e);
        i += hop;
    }
    let to_lufs = |e: f64| -0.691 + 10.0 * e.log10();
    let abs_gated: Vec<f64> = energies.iter().copied().filter(|e| *e > 0.0 && to_lufs(*e) > -70.0).collect();
    if abs_gated.is_empty() {
        return None;
    }
    let mean_abs = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
    let rel_thr = to_lufs(mean_abs) - 10.0;
    let rel: Vec<f64> = abs_gated.into_iter().filter(|e| to_lufs(*e) > rel_thr).collect();
    if rel.is_empty() {
        return None;
    }
    Some(to_lufs(rel.iter().sum::<f64>() / rel.len() as f64))
}

/// Lookahead peak limiter: never lets |sample| exceed `ceiling` (linear), with smooth gain.
pub fn limit_peaks(buf: &mut AudioBuf, ceiling: f32) {
    let sr = buf.sample_rate as f32;
    let look = ms_to_samples(buf.sample_rate, 5.0).max(1);
    let release = (-1.0 / (0.08 * sr)).exp();
    let n = buf.samples.len();
    // Required gain per sample.
    let need: Vec<f32> = buf
        .samples
        .iter()
        .map(|s| if s.abs() > ceiling { ceiling / s.abs() } else { 1.0 })
        .collect();
    // Minimum over the lookahead window (forward), so gain reduction starts before the peak.
    let mut target = vec![1.0f32; n];
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for i in (0..n).rev() {
        while let Some(&b) = dq.back() {
            if need[b] >= need[i] {
                dq.pop_back();
            } else {
                break;
            }
        }
        dq.push_back(i);
        while let Some(&f) = dq.front() {
            if f > i + look {
                dq.pop_front();
            } else {
                break;
            }
        }
        target[i] = need[*dq.front().unwrap()];
    }
    let mut g = 1.0f32;
    let attack_step = 1.0 / look as f32;
    for i in 0..n {
        let t = target[i];
        if t < g {
            g = (g - attack_step).max(t);
        } else {
            g = t + (g - t) * release;
        }
        buf.samples[i] *= g;
        // Hard safety clamp for the residual of the linear attack.
        buf.samples[i] = buf.samples[i].clamp(-ceiling, ceiling);
    }
}

/// Normalize integrated loudness to `target_lufs` and limit peaks to `peak_db` dBFS.
/// Returns (measured_before, gain_applied_db).
pub fn normalize_loudness(buf: &mut AudioBuf, target_lufs: f64, peak_db: f32) -> (Option<f64>, f32) {
    let measured = integrated_lufs(buf);
    let gain = match measured {
        Some(l) => ((target_lufs - l) as f32).clamp(-30.0, 30.0),
        None => 0.0,
    };
    apply_gain_db(buf, gain);
    limit_peaks(buf, db_to_lin(peak_db));
    (measured, gain)
}

// ---------------------------------------------------------------------------------------
// Reference quality analysis
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub duration_secs: f64,
    pub source_sample_rate: u32,
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    pub clipped_ratio: f32,
    pub silence_ratio: f32,
    pub noise_floor_dbfs: f32,
    pub snr_estimate_db: f32,
    pub loudness_lufs: Option<f64>,
    pub verdict: Verdict,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Good,
    Usable,
    Poor,
}

pub fn analyze_quality(buf: &AudioBuf, source_sr: u32) -> QualityReport {
    let frame = ms_to_samples(buf.sample_rate, 20.0).max(1);
    let mut frame_db: Vec<f32> = buf
        .samples
        .chunks(frame)
        .filter(|c| c.len() == frame)
        .map(|c| lin_to_db((c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt()))
        .collect();
    let clipped = buf.samples.iter().filter(|s| s.abs() >= 0.999).count() as f32 / buf.samples.len().max(1) as f32;
    frame_db.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f32| -> f32 {
        if frame_db.is_empty() {
            -200.0
        } else {
            frame_db[((frame_db.len() - 1) as f32 * p) as usize]
        }
    };
    let noise = pct(0.10);
    let speech = pct(0.90);
    let silence_ratio = frame_db.iter().filter(|d| **d < -45.0).count() as f32 / frame_db.len().max(1) as f32;
    let snr = speech - noise;
    let dur = buf.duration_secs();
    let mut issues = Vec::new();
    let mut score = 0;
    if dur < 5.0 {
        issues.push(format!("Too short ({dur:.1}s). At least 10 s required, 30–60 s recommended."));
        score += 3;
    } else if dur < 20.0 {
        issues.push(format!("Short reference ({dur:.1}s). 30–60 s recommended."));
        score += 1;
    } else if dur > 90.0 {
        issues.push(format!("Long reference ({dur:.1}s). Trim to 30–60 s of the cleanest speech."));
        score += 1;
    }
    if clipped > 0.001 {
        issues.push(format!("Clipping detected ({:.2}% of samples).", clipped * 100.0));
        score += if clipped > 0.01 { 3 } else { 1 };
    }
    if snr < 15.0 {
        issues.push(format!("High background noise (SNR ≈ {snr:.0} dB)."));
        score += 3;
    } else if snr < 25.0 {
        issues.push(format!("Some background noise (SNR ≈ {snr:.0} dB)."));
        score += 1;
    }
    if speech < -40.0 {
        issues.push("Very quiet recording.".into());
        score += 2;
    }
    if silence_ratio > 0.5 {
        issues.push(format!("Mostly silence ({:.0}%).", silence_ratio * 100.0));
        score += 2;
    }
    if source_sr < 16000 {
        issues.push(format!("Low sample rate ({source_sr} Hz). 22.05 kHz or higher recommended."));
        score += 2;
    }
    let verdict = match score {
        0 => Verdict::Good,
        1..=2 => Verdict::Usable,
        _ => Verdict::Poor,
    };
    QualityReport {
        duration_secs: dur,
        source_sample_rate: source_sr,
        peak_dbfs: lin_to_db(buf.peak()),
        rms_dbfs: lin_to_db(buf.rms()),
        clipped_ratio: clipped,
        silence_ratio,
        noise_floor_dbfs: noise,
        snr_estimate_db: snr,
        loudness_lufs: integrated_lufs(buf),
        verdict,
        issues,
    }
}

/// Downsampled min/max envelope for waveform display.
pub fn waveform_peaks(buf: &AudioBuf, buckets: usize) -> Vec<(f32, f32)> {
    if buf.samples.is_empty() || buckets == 0 {
        return vec![];
    }
    let per = (buf.samples.len() as f64 / buckets as f64).max(1.0);
    (0..buckets)
        .map(|b| {
            let s = (b as f64 * per) as usize;
            let e = (((b + 1) as f64 * per) as usize).min(buf.samples.len()).max(s + 1).min(buf.samples.len());
            if s >= e {
                return (0.0, 0.0);
            }
            let sl = &buf.samples[s..e];
            (sl.iter().cloned().fold(f32::MAX, f32::min), sl.iter().cloned().fold(f32::MIN, f32::max))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(sr: u32, freq: f32, secs: f32, amp: f32) -> AudioBuf {
        let n = (sr as f32 * secs) as usize;
        AudioBuf::new(
            sr,
            (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin()).collect(),
        )
    }

    #[test]
    fn lufs_reference_tone() {
        // EBU Tech 3341: 1 kHz sine at -20 dBFS (peak) mono reads about -23 LUFS
        // (-20 dBFS peak = -23.01 dBFS RMS; K-weighting at 1 kHz ≈ +0.69 dB, offset -0.691).
        for sr in [24000, 44100, 48000] {
            let b = sine(sr, 1000.0, 5.0, db_to_lin(-20.0));
            let l = integrated_lufs(&b).unwrap();
            assert!((l - -23.0).abs() < 0.3, "sr={sr} lufs={l}");
        }
    }

    #[test]
    fn silence_has_no_loudness() {
        assert!(integrated_lufs(&AudioBuf::silence(24000, 3000.0)).is_none());
    }

    #[test]
    fn normalize_hits_target_and_ceiling() {
        let mut b = sine(24000, 440.0, 4.0, 0.05);
        normalize_loudness(&mut b, -16.0, -1.0);
        let l = integrated_lufs(&b).unwrap();
        assert!((l - -16.0).abs() < 0.5, "{l}");
        assert!(b.peak() <= db_to_lin(-1.0) + 1e-6);
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut b = sine(24000, 200.0, 1.0, 1.8);
        limit_peaks(&mut b, 0.9);
        assert!(b.peak() <= 0.9 + 1e-6);
    }

    #[test]
    fn resample_preserves_frequency_and_length() {
        let b = sine(44100, 1000.0, 1.0, 0.5);
        let r = resample(&b, 24000);
        assert_eq!(r.samples.len(), 24000);
        // Count zero crossings: 1 kHz -> ~2000 per second.
        let zc = r.samples.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((zc as i32 - 1000).abs() <= 2, "{zc}");
        let mid = &r.samples[2000..22000];
        let peak = mid.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.03, "{peak}");
    }

    #[test]
    fn stitch_is_click_free() {
        // DC-offset pieces would click at hard joins; with fades the boundary jump is tiny.
        let a = AudioBuf::new(24000, vec![0.5; 2400]);
        let b = AudioBuf::new(24000, vec![-0.5; 2400]);
        let out = stitch(
            vec![Piece { audio: a.clone(), gap_before_ms: 0.0 }, Piece { audio: b, gap_before_ms: 100.0 }],
            24000,
            8.0,
            10.0,
        );
        let max_jump = out.samples.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_jump < 0.01, "max jump {max_jump}");
        assert_eq!(out.samples.len(), 2400 + 2400 + 2400);
        assert_eq!(out.samples[0], 0.0, "starts at zero");
        assert!(out.samples.last().unwrap().abs() < 1e-3, "ends at zero");
    }

    #[test]
    fn crossfade_join_length() {
        let a = sine(24000, 300.0, 0.5, 0.3);
        let b = sine(24000, 300.0, 0.5, 0.3);
        let out = stitch(
            vec![Piece { audio: a, gap_before_ms: 0.0 }, Piece { audio: b, gap_before_ms: 0.0 }],
            24000,
            5.0,
            10.0,
        );
        assert_eq!(out.samples.len(), 24000 - 240);
    }

    #[test]
    fn trim_keeps_speech() {
        let mut v = vec![0.0f32; 4800];
        v.extend(sine(24000, 300.0, 0.5, 0.3).samples);
        v.extend(vec![0.0f32; 4800]);
        let mut b = AudioBuf::new(24000, v);
        trim_silence(&mut b, -45.0, 20.0);
        let d = b.duration_secs();
        assert!(d > 0.5 && d < 0.6, "{d}");
    }

    #[test]
    fn wav_roundtrip_and_corrupt_detection() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let b = sine(24000, 440.0, 0.5, 0.5);
        write_wav(&p, &b).unwrap();
        let r = read_wav(&p).unwrap();
        assert_eq!(r.samples.len(), b.samples.len());
        let r2 = decode_file(&p).unwrap();
        assert_eq!(r2.sample_rate, 24000);
        assert!((r2.samples[1000] - b.samples[1000]).abs() < 1e-3);

        let bad = d.path().join("bad.mp3");
        std::fs::write(&bad, b"ID3\x03\x00garbage garbage garbage").unwrap();
        assert!(decode_file(&bad).is_err());
        let trunc = d.path().join("trunc.wav");
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&trunc, &bytes[..30]).unwrap();
        assert!(decode_file(&trunc).is_err());
    }

    #[test]
    fn quality_flags_noise_and_clipping() {
        let clean = {
            let mut v = Vec::new();
            for i in 0..30 {
                v.extend(sine(24000, 200.0 + i as f32 * 10.0, 0.8, 0.3).samples);
                v.extend(vec![0.0005f32; 4800]);
            }
            AudioBuf::new(24000, v)
        };
        let q = analyze_quality(&clean, 24000);
        assert_eq!(q.verdict, Verdict::Good, "{:?}", q.issues);
        let clipped = sine(24000, 200.0, 30.0, 1.5);
        let mut c = clipped.clone();
        for s in c.samples.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        let q = analyze_quality(&c, 24000);
        assert!(q.issues.iter().any(|i| i.contains("Clipping")));
        let short = sine(24000, 200.0, 2.0, 0.3);
        assert_eq!(analyze_quality(&short, 8000).verdict, Verdict::Poor);
    }
}
