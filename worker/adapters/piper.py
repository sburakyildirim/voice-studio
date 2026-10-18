"""Piper (piper-tts 1.8, GPL-3.0) adapter: fast multilingual ONNX voices, CPU friendly.

Voices live in the weights folder as <voice>.onnx + <voice>.onnx.json (any sub-folder).
No cloning and no emotion conditioning; Voice Studio maps markup to speaking rate.
"""
from pathlib import Path

import numpy as np


class Adapter:
    def __init__(self, weights_dir, device, status):
        from piper import PiperVoice  # noqa: F401  (import check)
        self.dir = Path(weights_dir)
        self.voices = {p.name[: -len(".onnx")]: p for p in self.dir.rglob("*.onnx")}
        if not self.voices:
            raise RuntimeError(f"no Piper voices found in {weights_dir}")
        self.loaded = {}
        # Piper's ONNX models are small; CPU is fast and avoids a CUDA onnxruntime build.
        self.device = "cpu"

    def _voice(self, name):
        from piper import PiperVoice
        if name not in self.voices:
            name = next(iter(sorted(self.voices)))
        if name not in self.loaded:
            self.loaded[name] = PiperVoice.load(str(self.voices[name]))
        return self.loaded[name]

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        from piper import SynthesisConfig
        v = self._voice(voice)
        cfg = SynthesisConfig(length_scale=1.0 / max(0.5, min(2.0, speed)), normalize_audio=True)
        chunks = list(v.synthesize(text, syn_config=cfg))
        if not chunks:
            return np.zeros(1, dtype="float32"), v.config.sample_rate
        sr = chunks[0].sample_rate
        gap = np.zeros(int(sr * 0.08), dtype="float32")
        parts = []
        for i, c in enumerate(chunks):
            if i:
                parts.append(gap)
            parts.append(np.asarray(c.audio_float_array, dtype="float32"))
        return np.concatenate(parts), sr
