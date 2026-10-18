"""Bark small (Suno, MIT) adapter via transformers.

Generative text-to-audio with preset speakers in 13 languages (including Turkish). No
cloning and no emotion input; Bark's own non-speech cues ([laughs], [sighs], …) can be written
escaped in the script (\\[laughs\\]). Each generation is limited to ~13 s, so segments are short.
"""
from pathlib import Path

import numpy as np


class Adapter:
    def __init__(self, weights_dir, device, status):
        import torch
        from transformers import AutoProcessor, BarkModel

        d = Path(weights_dir)
        self.device = device if device in ("cuda", "cpu") else "cpu"
        self.processor = AutoProcessor.from_pretrained(str(d))
        dtype = torch.float16 if self.device == "cuda" else torch.float32
        self.model = BarkModel.from_pretrained(str(d), torch_dtype=dtype).to(self.device).eval()
        self.sr = int(self.model.generation_config.sample_rate)
        self.torch = torch

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        inputs = self.processor(text, voice_preset=voice or "v2/en_speaker_6", return_tensors="pt")
        inputs = {k: (v.to(self.device) if hasattr(v, "to") else v) for k, v in inputs.items()}
        if "history_prompt" in inputs and isinstance(inputs["history_prompt"], dict):
            inputs["history_prompt"] = {k: v.to(self.device) for k, v in inputs["history_prompt"].items()}
        with self.torch.inference_mode():
            audio = self.model.generate(**inputs, do_sample=True)
        return audio.float().cpu().numpy().reshape(-1).astype("float32"), self.sr
