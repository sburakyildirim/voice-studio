# Benchmarks and verification (measured, not estimated)

**Machine:** Windows 11 Pro, Intel Core i7-13650HX (14 cores / 20 threads), 64 GB RAM
(commit limit ≈ 70 GB), NVIDIA RTX 4050 Laptop 6 GB. Measured 2026-10-16 with Voice Studio
0.1.0 (release build). **The machine was not idle**: Adobe Premiere Pro (7 GB), browsers and
other GPU users were running, which caused visible run-to-run variance and one out-of-memory
failure (see below). Numbers are reported as observed, including the bad runs.

Reproduce: `vscli bench [--engine …] [--device …] [--variant …]` (renders
`examples/scripts/benchmark_60s.txt` twice with the cache bypassed: a cold run that includes
model loading, then a warm run with the model loaded) and `scripts/verify_audio.py`.
RTF = processing time ÷ audio duration (lower is faster; < 1 is faster than real time).

## Speed and memory

| Engine / model | Device | Audio | Cold total (load + render) | Warm RTF | Peak RAM¹ | Notes |
|---|---|---|---|---|---|---|
| Kokoro fp32 | CPU, 8 threads | 50.9 s | 13.4–16.4 s | **0.24–0.30** | 1.15–1.23 GB | 3 runs |
| Kokoro 8-bit (q8) | CPU, 8 threads | 50.6 s | 42–48 s | 0.78–1.67 | 0.67–0.75 GB | slower than fp32 on this x86 CPU (ConvInteger kernels); worst run during heavy background load |
| Kokoro fp32 | DirectML (RTX 4050) | 3.5 s | 5.8 s | per-chunk 0.28 | 0.87 GB | ~5 s graph compile per load; not faster than CPU → "auto" uses CPU |
| Kokoro fp32 | "gpu" requested, DirectML session failed (GPU memory in use by other apps) | 50.9 s | 35.4 s | 0.61 | 1.23 GB | **automatic CPU fallback** worked; slow run under load |
| Chatterbox clone | CUDA (RTX 4050) | 50.0 s | 80.1 s | **1.02** | not captured² | VRAM (whole GPU) peak 3.7 GB |
| Chatterbox clone | CUDA, under memory pressure | 28.3 s / 15.6 s | — | 9.4 / 6.9 | 5.2–5.7 GB (app + worker) | system near commit limit, heavy paging |
| Chatterbox clone | CPU | 20.2 s | 200.7 s | **8.7** | not captured² | CPU-only is ~9× slower than real time |

¹ Peak resident memory of Voice Studio plus its worker processes, sampled every 100 ms.
² Early runs did not count the worker's grandchild process (uv's venv launcher starts the real
interpreter); fixed afterwards — the later Chatterbox rows include it (≈ 5.2–5.7 GB).

Kokoro first-load cost (ONNX session + lexicon) ≈ 0.5–1.5 s on CPU. Chatterbox first load
(worker start, torch import, 3.2 GB weights) ≈ 15–40 s; it is paid once per session and the
worker stays loaded until the idle timeout.

## Idle and start-up

| Measurement | Result |
|---|---|
| Idle CPU, whole process tree (app + WebView2), 3 × 20 s windows after start-up settled | **0.08–0.16 % of one core** |
| Idle CPU during the first ~40 s after launch (WebView2 initialisation) | ≈ 11–22 % of one core, then drops |
| Idle memory: app process | 32–41 MB RSS (no model loaded) |
| Idle memory: WebView2 process tree | ≈ 500 MB (system WebView, shared runtime) |
| Models loaded at start-up | none (loaded on first MAKE, unloaded after idle timeout) |

## Installer size

| Artifact | Size |
|---|---|
| NSIS installer (`Voice Studio_0.1.0_x64-setup.exe`) | **3.22 MiB** |
| MSI | 4.48 MiB |
| Portable `voice-studio.exe` | 11.6 MiB (12.2 MB) |
| `vscli.exe` | 4.4 MiB |

Models/runtimes (downloaded separately): Kokoro fp32 325.5 MB, voices 14.6 MB, lexicons 12.6
MB, ONNX Runtime DirectML 25.1 MB (CPU-only 14.9 MB); Chatterbox weights 3.19 GB, Python
runtime ≈ 5 GB on disk with CUDA torch.

## Intelligibility, voice similarity, stability

Transcribed with Whisper base.en; speaker similarity = cosine similarity of Chatterbox's
VoiceEncoder embeddings (1.0 = identical). The cloning reference is
`examples/reference/synthetic_kokoro_bf_emma.wav`, a 41 s synthetic voice generated with
Kokoro (no real person).

| Render | WER | Similarity to reference | Segment stability (min / mean)³ |
|---|---|---|---|
| Kokoro af_heart, 60 s narration (q8) | 1.3 % (one "sea floor" → "seafloor") | — | 0.902 / 0.947 |
| Chatterbox **clone**, Shorts hook (7 segments) | 0.0 % | **0.938** | 0.892 / 0.917 |
| Chatterbox **built-in voice**, same text (baseline) | — | 0.616 | — |
| Chatterbox clone, emotion contrast (offline) | 0.0 % | 0.949 | 0.960 / 0.970 |
| Kokoro, emotion contrast (offline) | 0.0 % | — | 0.975 / 0.981 |

³ Cosine similarity of each segment's embedding to the mean of all segments of the render.

Cloning works measurably (0.94 vs 0.62 for a non-cloned voice), but this is one synthetic
reference; similarity to real recordings will vary and exact matching is never promised.

## Does the markup change delivery?

Same sentence rendered four times (`examples/scripts/emotion_same_sentence.txt`), one segment
per emotion, single seed — small sample, indicative only:

| | words/s | level (dBFS) | median f0 (Hz) | f0 spread (Hz) |
|---|---|---|---|---|
| **Kokoro** neutral | 4.29 | −22.5 | 195.2 | 60.1 |
| Kokoro excited | 4.48 | −21.1 | 190.8 | 61.7 |
| Kokoro calm | 4.10 | −23.9 | 190.4 | 62.7 |
| Kokoro mystery | 3.84 | −25.4 | 190.2 | 62.9 |
| **Chatterbox** neutral | 5.05 | −18.4 | 176.9 | 49.6 |
| Chatterbox excited | 5.06 | −17.3 | **188.8** | **57.4** |
| Chatterbox calm | 4.56 | −19.5 | 175.5 | 57.5 |
| Chatterbox mystery | 4.71 | −20.1 | 178.3 | 58.9 |

Kokoro's changes are exactly the approximations applied (rate, level); pitch is essentially
unchanged — consistent with the "approximated" label. Chatterbox's exaggeration control raises
pitch and pitch variation for excited delivery (+12 Hz median, +16 % spread) and slows calm
delivery; effects are real but moderate.

## Robustness checks performed

| Check | How | Result |
|---|---|---|
| Interrupted download | killed `vscli install kokoro-82m-fp32` at 6.7 MB of 325 MB, re-ran | resumed via HTTP Range, SHA-256 verified |
| Download resume / checksum (unit) | local HTTP server drops the connection mid-file; wrong hash | resumes; mismatched file rejected and deleted |
| Model absence | render with an empty model folder | precise `MISSING_PACKAGES:…` → UI opens Options › Models |
| Offline operation | all HTTP(S) forced to a dead proxy (`127.0.0.1:9`) | Kokoro and Chatterbox clone renders succeeded |
| Orphaned processes | hard-killed the parent while Chatterbox was loading | both worker processes terminated (Windows Job Object) |
| Worker lifecycle | `unload_all()` then process lookup | worker gone (e2e test) |
| Out of memory | system commit limit reached while loading Chatterbox | worker crashed, app unaffected, error now explains the cause |
| Unsupported device | "gpu" requested while DirectML could not create a session | automatic CPU fallback |
| Corrupt audio | garbage MP3, truncated WAV, tampered profile reference | rejected with clear errors (unit + e2e) |
| Batch restart | queue with a job marked running, process "crashes", queue reloaded | job resumed, all 3 jobs done (e2e) |
| Cache | unchanged script / one edited sentence / different voice | all cached / n−1 cached / none cached (e2e) |
| Cancel | cancel after 2 segments | stops, no half-written project (e2e) |
| Markup | 25 parser tests incl. 2,000-case fuzz of word preservation | pass |

Test totals: **75 unit tests** and **7 end-to-end tests** with real models pass on Windows x64.

## Not measured / not verified

* macOS (Intel, Apple Silicon) and Linux: builds and tests are configured in CI but were not
  run by the author. The macOS code type-checks for both Mac targets; CoreML and MPS paths are
  untested. Chatterbox is intentionally unavailable on Intel Macs (no PyTorch 2.6 wheels).
* Low-power hardware (old Intel, base Apple Silicon): not tested.
* Naturalness (MOS) — no listening study was run, so the catalog does not score it.
* App cold start to first paint was not instrumented (the engine load times above are).
