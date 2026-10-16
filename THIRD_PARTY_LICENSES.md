# Third-party components

Voice Studio's own source code is MIT licensed (see `LICENSE`). The installer contains only
Voice Studio's code and the Rust crates it is compiled from. Models and runtimes are **not**
bundled; they are downloaded on demand from their original publishers, verified by SHA-256,
and keep their own licenses:

| Component | Where it comes from | License | Notes |
|-----------|--------------------|---------|-------|
| Kokoro 82M v1.0 (ONNX export) | huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX (from hexgrad/Kokoro-82M) | Apache-2.0 | model + 28 English voices |
| misaki English lexicons (`us_gold/us_silver/gb_gold/gb_silver.json`) | github.com/hexgrad/misaki @ fba1236 | Apache-2.0 | pronunciation dictionaries; Voice Studio's G2P is a Rust port of misaki's lexicon logic |
| ONNX Runtime 1.24.4 DirectML / 1.31.0 / 1.23.2 (macOS x64) | PyPI wheels and GitHub releases of microsoft/onnxruntime | MIT | DirectML.dll is redistributed by Microsoft inside the wheel under its own license terms |
| Chatterbox (English) weights | huggingface.co/ResembleAI/chatterbox @ 5bb1f6e | MIT | output carries Resemble AI's Perth watermark |
| chatterbox-tts 0.1.7 + dependencies (PyTorch 2.6 BSD-3, transformers Apache-2.0, librosa ISC, resemble-perth, etc.) | PyPI / download.pytorch.org, installed into an isolated venv | various permissive | installed by uv only when the user installs the runtime |
| uv 0.13.1 | github.com/astral-sh/uv | MIT OR Apache-2.0 | only downloaded if uv is not already installed |
| Python 3.11 (python-build-standalone) | fetched by uv | PSF | |
| FFmpeg (optional, user-installed) | not distributed | LGPL/GPL | used for MP3 export only if found |

Letter-to-sound fallback rules are adapted from *Automatic Translation of English Text to
Phonetics by Means of Letter-to-Sound Rules* (Elovitz, Johnson, McHugh, Shore; NRL Report
7948, 1976), a US government work in the public domain.

Rust crates (compiled in): tauri, tauri-plugin-dialog, tauri-plugin-opener (MIT/Apache-2.0),
ort (MIT/Apache-2.0), ndarray, serde, serde_json, anyhow, chrono, uuid, sha2, hex, hound
(Apache-2.0), symphonia (MPL-2.0), ureq + rustls (MIT/Apache-2.0/ISC), zip, flate2, tar,
sysinfo, unicode-normalization, tracing, tracing-subscriber, tracing-appender, dirs,
windows-sys. Run `cargo tree --edges normal` for the complete list; all are MIT, Apache-2.0,
BSD, ISC, Zlib, Unicode-3.0 or MPL-2.0.

Front-end npm packages: @tauri-apps/api, @tauri-apps/plugin-dialog, @tauri-apps/plugin-opener
(MIT/Apache-2.0); build-time only: vite (MIT), typescript (Apache-2.0), @tauri-apps/cli.

The model catalog in Options › Models lists 24 evaluated models with the license and
commercial-use terms stated by their publishers on 2026-10-16. Several (F5-TTS, XTTS-v2,
OpenAudio S1-mini, Spark-TTS, MaskGCT) are **non-commercial** and are deliberately not
integrated. Always check the current terms at the source.
