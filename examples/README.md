# Examples

* `scripts/` — example scripts: `shorts_hook.txt` (tagged Shorts narration), `benchmark_60s.txt`
  (≈ 50–60 s narration used by `vscli bench`), `emotion_contrast.txt`, `markup_edge_cases.txt`,
  `batch_example.txt` (three scripts separated by `===`), `synthetic_reference.txt`.
* `reference/synthetic_kokoro_bf_emma.wav` — a 41 s **synthetic** reference voice generated
  by Voice Studio with Kokoro's `bf_emma` voice (Apache-2.0). It is used as the authorized
  reference for the cloning demo: no real person's voice is involved.
* `demo/` — real outputs rendered by this build:
  * `kokoro_af_heart_60s_narration.wav` — `benchmark_60s.txt`, Kokoro, voice af_heart.
  * `chatterbox_clone_synthetic_emma.wav` — `shorts_hook.txt`, Chatterbox Voice Only clone
    of the synthetic reference above (CUDA).
  * `chatterbox_builtin_voice_same_text.wav` — the same text with Chatterbox's built-in
    voice, for comparison. Objective numbers are in `docs/BENCHMARKS.md`.
