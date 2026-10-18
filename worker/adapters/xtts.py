"""XTTS-v2 (Coqui, CPML non-commercial) adapter via the maintained coqui-tts package.

Zero-shot cloning from a reference clip, 17 languages, built-in studio speakers.
No emotion input: Voice Studio maps markup to the model's speed parameter and pauses.
"""
import os
from pathlib import Path

import numpy as np

# Local checkpoint only; never prompt for the model license on stdin.
os.environ.setdefault("COQUI_TOS_AGREED", "1")


class Adapter:
    def __init__(self, weights_dir, device, status):
        import torch
        from TTS.tts.configs.xtts_config import XttsConfig
        from TTS.tts.models.xtts import Xtts

        d = Path(weights_dir)
        cfg = XttsConfig()
        cfg.load_json(str(d / "config.json"))
        self.model = Xtts.init_from_config(cfg)
        self.model.load_checkpoint(cfg, checkpoint_dir=str(d), use_deepspeed=False,
                                   speaker_file_path=str(d / "speakers_xtts.pth"))
        self.device = device if device in ("cuda", "cpu") else "cpu"
        if self.device == "cuda":
            self.model.cuda()
        self.model.eval()
        self.torch = torch
        self.latents = {}

    def _conditioning(self, voice, ref):
        key = ("ref", ref, os.path.getmtime(ref)) if ref else ("spk", voice)
        if key not in self.latents:
            if ref:
                g, s = self.model.get_conditioning_latents(audio_path=[ref])
            else:
                spk = self.model.speaker_manager.speakers
                name = voice if voice in spk else next(iter(spk))
                g, s = spk[name]["gpt_cond_latent"], spk[name]["speaker_embedding"]
            self.latents[key] = (g, s)
        return self.latents[key]

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        g, s = self._conditioning(voice, ref)
        with self.torch.inference_mode():
            out = self.model.inference(text, (language or "en"), g, s, temperature=0.7,
                                       speed=float(max(0.6, min(1.6, speed))), enable_text_splitting=False)
        wav = out["wav"]
        if hasattr(wav, "cpu"):
            wav = wav.cpu().numpy()
        return np.asarray(wav, dtype="float32"), 24000
