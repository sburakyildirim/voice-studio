"""StyleTTS 2 LibriTTS (code MIT; pretrained-model usage condition) adapter via `styletts2`.

Zero-shot style/voice transfer from a reference clip. Phonemes come from gruut (no espeak).
Markup is approximated through StyleTTS 2's expressiveness controls (embedding scale and the
reference/prediction style mix), not emotion categories. Utility checkpoints (ASR, F0, PL-BERT)
are fetched once at install into the runtime's assets folder; the NLTK data too.
"""
import os
import sys
import zipfile
from pathlib import Path

import numpy as np

ASSETS = Path(sys.prefix).parent / "assets"
GH = "https://github.com/yl4579/StyleTTS2/raw/main/"
STYLE = {  # alpha (timbre from reference), beta (prosody from reference), embedding_scale
    # embedding_scale > ~1.5 makes short lines unstable (noise), so the range is kept narrow.
    "neutral": (0.3, 0.7, 1.0),
    "excited": (0.3, 0.8, 1.15),
    "calm": (0.3, 0.5, 1.0),
    "mystery": (0.3, 0.6, 1.1),
}


class Adapter:
    def __init__(self, weights_dir, device, status):
        import nltk
        nltk.download = lambda *a, **k: True  # data is pre-installed; never go online
        nltk.data.path.insert(0, str(ASSETS / "nltk_data"))
        import torch
        # These are the official, revision-pinned and SHA-256-verified checkpoints, saved
        # with full pickles before PyTorch 2.6 made weights_only the default.
        _load = torch.load
        torch.load = lambda *a, **k: _load(*a, **{**k, "weights_only": k.get("weights_only", False)})
        from styletts2 import tts as st

        local = {
            GH + "Utils/ASR/epoch_00080.pth": ASSETS / "ASR" / "epoch_00080.pth",
            GH + "Utils/ASR/config.yml": ASSETS / "ASR" / "config.yml",
            GH + "Utils/JDC/bst.t7": ASSETS / "JDC" / "bst.t7",
            GH + "Utils/PLBERT/step_1000000.t7": ASSETS / "PLBERT" / "step_1000000.t7",
            GH + "Utils/PLBERT/config.yml": ASSETS / "PLBERT" / "config.yml",
        }

        def cached(url, *a, **k):
            key = url.split("?")[0]
            if key in local:
                return str(local[key])
            raise RuntimeError(f"offline: no local copy of {url}")

        st.cached_path = cached
        d = Path(weights_dir)
        self.model = st.StyleTTS2(model_checkpoint_path=str(d / "Models/LibriTTS/epochs_2nd_00020.pth"),
                                  config_path=str(d / "Models/LibriTTS/config.yml"))
        import torch as _t
        self.device = "cuda" if _t.cuda.is_available() else "cpu"
        refs = d / "reference_audio"
        if not refs.exists():
            with zipfile.ZipFile(d / "reference_audio.zip") as z:
                z.extractall(refs)
        self.builtin = str(sorted(refs.rglob("*.wav"))[0])
        self.styles = {}

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        target = ref or self.builtin
        key = (target, os.path.getmtime(target))
        if key not in self.styles:
            self.styles[key] = self.model.compute_style(target)
        alpha, beta, scale = STYLE.get(emotion, STYLE["neutral"])
        # gruut emits a phoneme StyleTTS 2 does not know for '!' (and ';'), which drops the
        # sentence: normalise those to plain punctuation.
        clean = text.replace("!", ".").replace(";", ",").replace("?.", "?")
        words = max(1, len(clean.split()))
        max_secs = 0.6 * words + 1.5
        for attempt in range(3):
            wav = self.model.inference(clean, ref_s=self.styles[key], alpha=alpha, beta=beta,
                                       diffusion_steps=10, embedding_scale=scale)
            y = np.asarray(wav, dtype="float32").reshape(-1)
            secs = len(y) / 24000
            rms = float(np.sqrt(np.mean(y ** 2))) if len(y) else 0.0
            # The diffusion sampler occasionally diverges on short lines (very long or very
            # loud output); retry with the neutral, stable setting.
            if secs <= max_secs and rms < 0.35:
                return y, 24000
            alpha, beta, scale = STYLE["neutral"]
        return y, 24000
