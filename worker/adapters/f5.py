"""F5-TTS v1 (SWivid; code MIT, weights CC-BY-NC-4.0) adapter.

Flow-matching zero-shot cloning. F5 needs the transcript of its reference clip, so the
reference is cut to <= 11 s at a pause and transcribed locally with Whisper-base (shipped in
the weights folder) — no network access. Without a profile, the packaged English example
voice is used.
"""
from pathlib import Path
import os
import tempfile

import numpy as np

BUILTIN_TEXT = "Some call me nature, others call me mother nature."


class Adapter:
    def __init__(self, weights_dir, device, status):
        from f5_tts.api import F5TTS
        from importlib.resources import files

        d = Path(weights_dir)
        self.dir = d
        self.device = device if device in ("cuda", "cpu", "mps") else "cpu"
        self.tts = F5TTS(
            model="F5TTS_v1_Base",
            ckpt_file=str(d / "F5TTS_v1_Base" / "model_1250000.safetensors"),
            vocab_file=str(d / "F5TTS_v1_Base" / "vocab.txt"),
            vocoder_local_path=str(d / "vocos"),
            device=self.device,
        )
        self.builtin = str(files("f5_tts").joinpath("infer/examples/basic/basic_ref_en.wav"))
        self.asr = None
        self.refs = {}
        self.tmp = Path(tempfile.gettempdir()) / "voice-studio-f5"
        self.tmp.mkdir(exist_ok=True)

    def _transcribe(self, wav_path):
        # Whisper is called directly: transformers' ASR pipeline imports torchcodec, whose
        # wheels do not match this runtime's PyTorch build.
        import librosa
        import torch
        from transformers import WhisperForConditionalGeneration, WhisperProcessor
        if self.asr is None:
            proc = WhisperProcessor.from_pretrained(str(self.dir / "whisper"))
            model = WhisperForConditionalGeneration.from_pretrained(str(self.dir / "whisper")).to(self.device).eval()
            self.asr = (proc, model)
        proc, model = self.asr
        audio, _ = librosa.load(wav_path, sr=16000, mono=True)
        feats = proc(audio, sampling_rate=16000, return_tensors="pt").input_features.to(self.device)
        with torch.inference_mode():
            ids = model.generate(feats, task="transcribe")
        return proc.batch_decode(ids, skip_special_tokens=True)[0].strip()

    def _reference(self, ref):
        """(clip_path, transcript) for a profile reference, cached per file version."""
        key = (ref, os.path.getmtime(ref))
        if key in self.refs:
            return self.refs[key]
        import librosa
        import soundfile as sf
        y, sr = librosa.load(ref, sr=24000, mono=True)
        max_len = int(sr * 11)
        if len(y) > max_len:
            # Cut at the quietest 50 ms window between 7 and 11 s so no word is split.
            hop = int(sr * 0.05)
            start = int(sr * 7)
            frames = [np.sqrt(np.mean(y[i:i + hop] ** 2)) for i in range(start, max_len - hop, hop)]
            cut = start + int(np.argmin(frames)) * hop if frames else max_len
            y = y[:cut]
        clip = self.tmp / f"ref-{abs(hash(key))}.wav"
        sf.write(str(clip), y, sr)
        text = self._transcribe(str(clip))
        self.refs[key] = (str(clip), text)
        return self.refs[key]

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        if ref:
            ref_file, rtext = self._reference(ref)
        else:
            ref_file, rtext = self.builtin, BUILTIN_TEXT
        import soundfile as sf
        speed = float(max(0.6, min(1.6, speed)))
        ref_secs = sf.info(ref_file).duration
        # F5 sizes the output from the reference's text/audio ratio, which under-estimates
        # short sentences (words get swallowed). Use a per-word floor instead.
        by_ratio = ref_secs / max(1, len(rtext.encode("utf-8"))) * len(text.encode("utf-8")) / speed
        by_words = 0.42 * len(text.split()) / speed + 0.3
        wav, sr, _ = self.tts.infer(
            ref_file, rtext, text,
            show_info=lambda *a, **k: None,
            speed=speed,
            seed=int(seed),
            remove_silence=False,
            fix_duration=ref_secs + max(by_ratio, by_words),
        )
        return np.asarray(wav, dtype="float32"), int(sr)
