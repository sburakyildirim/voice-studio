"""Spark-TTS 0.5B (SparkAudio; code Apache-2.0, weights CC-BY-NC-SA-4.0) adapter.

Two modes: zero-shot cloning from a reference clip, or "voice creation" from attributes
(gender, pitch level, speed level). In creation mode the markup emotions are approximated
through those attribute levels; with a reference they only change pauses.
"""
from pathlib import Path

import numpy as np

LEVELS = {  # pitch, speed
    "neutral": ("moderate", "moderate"),
    "excited": ("high", "high"),
    "calm": ("low", "low"),
    "mystery": ("very_low", "low"),
}


class Adapter:
    def __init__(self, weights_dir, device, status):
        import torch
        from spark_cli.SparkTTS import SparkTTS

        self.device = device if device in ("cuda", "cpu") else "cpu"
        self.model = SparkTTS(Path(weights_dir), torch.device("cuda:0" if self.device == "cuda" else "cpu"))
        self.sr = int(self.model.sample_rate)
        self.torch = torch

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        with self.torch.inference_mode():
            if ref:
                wav = self.model.inference(text, prompt_speech_path=Path(ref), prompt_text=None)
            else:
                pitch, rate = LEVELS.get(emotion, LEVELS["neutral"])
                if speed > 1.15 and rate == "moderate":
                    rate = "high"
                elif speed < 0.87 and rate == "moderate":
                    rate = "low"
                wav = self.model.inference(text, gender=(voice or "female"), pitch=pitch, speed=rate)
        return np.asarray(wav, dtype="float32").reshape(-1), self.sr
