"""Kyutai TTS 1.6B en/fr (CC-BY-4.0) adapter via the moshi package.

Voices are pre-computed embeddings from kyutai/tts-voices. For the two Expresso speakers,
Voice Studio picks the same speaker's recording in the matching style for each markup emotion
(narration / happy / calm / whisper-or-awe), so emotions are genuine style conditioning.
User reference cloning is not possible: Kyutai has not released the voice encoder.
"""
from pathlib import Path

import numpy as np

SUFFIX = ".1e68beda@240.safetensors"
STYLES = {
    "ex04": {
        "neutral": "expresso/ex04-ex01_narration_001_channel1_605s.wav",
        "excited": "expresso/ex04-ex02_happy_001_channel1_118s.wav",
        "calm": "expresso/ex04-ex02_calm_002_channel1_480s.wav",
        "mystery": "expresso/ex04-ex03_whisper_001_channel1_198s.wav",
    },
    "ex03": {
        "neutral": "expresso/ex03-ex02_narration_001_channel1_674s.wav",
        "excited": "expresso/ex03-ex01_happy_001_channel1_334s.wav",
        "calm": "expresso/ex03-ex01_calm_001_channel1_1143s.wav",
        "mystery": "expresso/ex03-ex01_awe_001_channel1_1323s.wav",
    },
}


class Adapter:
    def __init__(self, weights_dir, device, status):
        import torch
        from moshi.models import loaders
        from moshi.models.tts import TTSModel

        d = Path(weights_dir)
        self.dir = d

        def local(repo_id, filename=None, **kw):
            # Everything was downloaded and verified into the model folder.
            if repo_id and "tts-voices" in repo_id:
                return str(d / "voices" / filename)
            return str(d / filename)

        loaders.hf_hub_download = local
        self.device = device if device in ("cuda", "cpu") else "cpu"
        info = loaders.CheckpointInfo.from_hf_repo("kyutai/tts-1.6b-en_fr")
        self.model = TTSModel.from_checkpoint_info(info, n_q=32, temp=0.6, device=torch.device(self.device))
        self.sr = int(self.model.mimi.sample_rate)
        self.torch = torch

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        styles = STYLES.get(voice or "ex04", STYLES["ex04"])
        voice_file = self.dir / "voices" / (styles.get(emotion, styles["neutral"]) + SUFFIX)
        entries = self.model.prepare_script([text], padding_between=1)
        attrs = self.model.make_condition_attributes([voice_file], cfg_coef=2.0)
        pcms = []

        def on_frame(frame):
            if (frame != -1).all():
                pcm = self.model.mimi.decode(frame[:, 1:, :]).cpu().numpy()
                pcms.append(np.clip(pcm[0, 0], -1, 1))

        with self.torch.inference_mode(), self.model.mimi.streaming(1):
            self.model.generate([entries], [attrs], on_frame=on_frame)
        audio = np.concatenate(pcms, axis=-1) if pcms else np.zeros(1, dtype="float32")
        return audio.astype("float32"), self.sr
