"""Dia 1.6B (Nari Labs, Apache-2.0) adapter via transformers' DiaForConditionalGeneration.

Dialogue-style English TTS with expressive non-verbal cues written in parentheses, e.g.
(laughs), (sighs), (gasps). Every segment is spoken by speaker [S1]; the voice is chosen by the
seed. No reference cloning in this integration. Loaded in float16 to fit 6 GB GPUs.
"""
from pathlib import Path

import numpy as np


class Adapter:
    def __init__(self, weights_dir, device, status):
        import torch
        from transformers import (DacModel, DiaFeatureExtractor, DiaForConditionalGeneration,
                                  DiaProcessor, DiaTokenizer)

        d = Path(weights_dir)
        self.device = device if device in ("cuda", "cpu") else "cpu"
        self.processor = DiaProcessor(
            feature_extractor=DiaFeatureExtractor.from_pretrained(str(d)),
            tokenizer=DiaTokenizer.from_pretrained(str(d)),
            audio_tokenizer=DacModel.from_pretrained(str(d / "dac")),
        )
        dtype = torch.float16 if self.device == "cuda" else torch.float32
        self.model = DiaForConditionalGeneration.from_pretrained(str(d), torch_dtype=dtype).to(self.device).eval()
        self.processor.audio_tokenizer.to(self.device)
        self.sr = int(self.processor.feature_extractor.sampling_rate)
        self.torch = torch

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        prompt = text if text.lstrip().startswith("[S") else f"[S1] {text}"
        inputs = self.processor(text=[prompt], padding=True, return_tensors="pt").to(self.device)
        # With its recommended sampling (temperature 1.8) Dia ends lines by itself; the cap
        # (generous: ~0.8 s per word + 3 s, 86 audio frames per second) only stops runaways.
        words = len(text.split()) + 2 * text.count("(")
        max_tokens = int(86 * min(25.0, 3.0 + 0.8 * words / max(0.6, speed)))
        with self.torch.inference_mode():
            out = self.model.generate(**inputs, max_new_tokens=max_tokens, guidance_scale=3.0,
                                      temperature=1.8, top_p=0.90, top_k=45)
            audio = self.processor.batch_decode(out)
        a = audio[0]
        if hasattr(a, "cpu"):
            a = a.float().cpu().numpy()
        return np.asarray(a, dtype="float32").reshape(-1), self.sr
