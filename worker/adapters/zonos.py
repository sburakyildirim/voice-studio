"""Zonos v0.1 transformer (Zyphra, Apache-2.0) adapter.

Genuine emotion conditioning: Voice Studio's markup maps onto Zonos' 8-dimensional emotion
vector plus pitch variation and speaking rate. Zero-shot cloning via its speaker embedding.
Upstream only supports Linux; on Windows/macOS the pure-PyTorch transformer backbone is used
and espeak-ng comes from the espeakng-loader wheel. Hub downloads are redirected to the local
model folder so it works offline.
"""
import os
from pathlib import Path

import numpy as np

#            Happiness, Sadness, Disgust, Fear, Surprise, Anger, Other, Neutral
EMOTION = {
    "neutral": ([0.05, 0.05, 0.02, 0.02, 0.05, 0.02, 0.1, 0.9], 30.0, 14.0),
    "excited": ([0.9, 0.02, 0.02, 0.02, 0.6, 0.05, 0.2, 0.1], 90.0, 17.0),
    "calm": ([0.15, 0.1, 0.02, 0.02, 0.02, 0.02, 0.1, 0.95], 18.0, 11.5),
    "mystery": ([0.02, 0.2, 0.02, 0.55, 0.15, 0.02, 0.4, 0.3], 40.0, 11.0),
}


class Adapter:
    def __init__(self, weights_dir, device, status):
        import espeakng_loader
        from phonemizer.backend.espeak.wrapper import EspeakWrapper

        EspeakWrapper.set_library(espeakng_loader.get_library_path())
        if hasattr(EspeakWrapper, "set_data_path"):
            EspeakWrapper.set_data_path(espeakng_loader.get_data_path())
        os.environ.setdefault("ESPEAK_DATA_PATH", espeakng_loader.get_data_path())

        d = Path(weights_dir)
        import zonos.autoencoder as ae
        import zonos.speaker_cloning as sc

        # Redirect Hub lookups to the verified local copies.
        def local_hub(repo_id=None, filename=None, **kw):
            return str(d / "speaker" / filename)

        sc.hf_hub_download = local_hub
        orig = ae.DacModel.from_pretrained

        def dac_local(name, *a, **kw):
            return orig(str(d / "dac") if name == "descript/dac_44khz" else name, *a, **kw)

        ae.DacModel.from_pretrained = dac_local

        import torch
        from zonos.model import Zonos

        self.device = device if device in ("cuda", "cpu") else "cpu"
        self.model = Zonos.from_local(str(d / "config.json"), str(d / "model.safetensors"), device=self.device)
        self.model.requires_grad_(False).eval()
        self.torch = torch
        self.speakers = {}

    def _speaker(self, ref):
        if not ref:
            return None
        key = (ref, os.path.getmtime(ref))
        if key not in self.speakers:
            import torchaudio
            wav, sr = torchaudio.load(ref)
            self.speakers[key] = self.model.make_speaker_embedding(wav, sr)
        return self.speakers[key]

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        from zonos.conditioning import make_cond_dict

        vec, pitch_std, rate = EMOTION.get(emotion, EMOTION["neutral"])
        cond = make_cond_dict(
            text=text,
            speaker=self._speaker(ref),
            language=language or "en-us",
            emotion=vec,
            pitch_std=pitch_std,
            speaking_rate=rate * float(max(0.6, min(1.6, speed))),
            device=self.device,
        )
        with self.torch.inference_mode():
            conditioning = self.model.prepare_conditioning(cond)
            codes = self.model.generate(conditioning, max_new_tokens=86 * 30, progress_bar=False, disable_torch_compile=True)
            wav = self.model.autoencoder.decode(codes).float().cpu().numpy().squeeze()
        return np.asarray(wav, dtype="float32"), int(self.model.autoencoder.sampling_rate)
