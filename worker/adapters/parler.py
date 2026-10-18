"""Parler-TTS mini v1.1 (Hugging Face, Apache-2.0) adapter.

Delivery is controlled by a natural-language description, so excited / calm / mystery are
applied by the model itself (genuine conditioning; adherence varies). The voice is one of the
named training speakers. No reference cloning.
"""
from pathlib import Path

import numpy as np

STYLE = {
    "neutral": "speaks at a moderate pace with a clear, natural and neutral tone",
    "excited": "speaks quickly and energetically with an excited, enthusiastic, animated tone and lively, expressive pitch",
    "calm": "speaks slowly and calmly in a soft, gentle, relaxed and soothing tone with a slightly low pitch",
    "mystery": "speaks slowly in a low, hushed, mysterious and suspenseful tone, almost whispering",
}
QUALITY = "The recording is of very high quality, with the speaker's voice sounding clear and very close up, with no background noise."


class Adapter:
    def __init__(self, weights_dir, device, status):
        import torch
        from parler_tts import ParlerTTSForConditionalGeneration
        from transformers import AutoTokenizer

        d = Path(weights_dir)
        self.device = device if device in ("cuda", "cpu", "mps") else "cpu"
        dtype = torch.float16 if self.device == "cuda" else torch.float32
        self.model = ParlerTTSForConditionalGeneration.from_pretrained(str(d), torch_dtype=dtype).to(self.device).eval()
        self.prompt_tok = AutoTokenizer.from_pretrained(str(d))
        self.desc_tok = AutoTokenizer.from_pretrained(str(d / "desc_tokenizer"))
        self.sr = self.model.config.sampling_rate
        self.torch = torch

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        speaker = voice or "Jon"
        desc = f"{speaker} {STYLE.get(emotion, STYLE['neutral'])}. {QUALITY}"
        d = self.desc_tok(desc, return_tensors="pt").to(self.device)
        p = self.prompt_tok(text, return_tensors="pt").to(self.device)
        with self.torch.inference_mode():
            gen = self.model.generate(
                input_ids=d.input_ids, attention_mask=d.attention_mask,
                prompt_input_ids=p.input_ids, prompt_attention_mask=p.attention_mask,
                do_sample=True, temperature=1.0,
            )
        audio = gen.float().cpu().numpy().squeeze()
        return np.asarray(audio, dtype="float32"), int(self.sr)
