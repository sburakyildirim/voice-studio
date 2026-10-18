"""CosyVoice 2 0.5B (Alibaba FunAudioLLM, Apache-2.0) adapter.

Uses instruct mode: the delivery instruction is given to the model in natural language, so
excited / calm / mystery are genuine conditioning (adherence varies). The speaker comes from
a reference clip (Voice Clone) or the packaged zero-shot prompt voice; no transcript needed.
Languages: Chinese, English, Japanese, Korean and Chinese dialects.
"""
import os
import site
from pathlib import Path

import numpy as np

INSTRUCT = {
    "neutral": "用自然、清晰的语气说这句话<|endofprompt|>",
    "excited": "用非常激动、兴奋、充满活力的语气说这句话<|endofprompt|>",
    "calm": "用平静、温柔、舒缓的语气慢慢地说这句话<|endofprompt|>",
    "mystery": "用低沉、神秘、悬疑的语气小声地说这句话<|endofprompt|>",
}


class Adapter:
    def __init__(self, weights_dir, device, status):
        from cosyvoice.cli.cosyvoice import CosyVoice2

        self.device = "cuda" if device == "cuda" else "cpu"
        self.model = CosyVoice2(str(Path(weights_dir)), load_jit=False, load_trt=False, fp16=False)
        sp = [p for p in site.getsitepackages() if p.endswith("site-packages")][0]
        self.builtin = os.path.join(sp, "cosyvoice_assets", "zero_shot_prompt.wav")
        self.sr = self.model.sample_rate
        self.prompts = {}

    def _prompt(self, ref):
        """16 kHz prompt clip (<= 15 s) for the reference, cached per file version."""
        path = ref or self.builtin
        key = (path, os.path.getmtime(path))
        if key not in self.prompts:
            import librosa
            import soundfile as sf
            import tempfile
            y, _ = librosa.load(path, sr=16000, mono=True)
            y = y[: 16000 * 15]
            out = Path(tempfile.gettempdir()) / f"vs-cosy-{abs(hash(key))}.wav"
            sf.write(str(out), y, 16000)
            self.prompts[key] = str(out)
        return self.prompts[key]

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        prompt = self._prompt(ref)
        chunks = []
        for out in self.model.inference_instruct2(text, INSTRUCT.get(emotion, INSTRUCT["neutral"]), prompt,
                                                  stream=False, speed=float(max(0.6, min(1.6, speed)))):
            chunks.append(out["tts_speech"].float().cpu().numpy().reshape(-1))
        audio = np.concatenate(chunks) if chunks else np.zeros(1, dtype="float32")
        return audio.astype("float32"), int(self.sr)
