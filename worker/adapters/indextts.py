"""IndexTTS-2 (bilibili; custom model license) adapter.

The only integrated model with separate speaker and emotion control: the speaker comes from
the voice profile (Voice Clone is required), the emotion from IndexTTS-2's 8-dimensional
emotion vector [happy, angry, sad, afraid, disgusted, melancholic, surprised, calm], driven by
the markup. Auxiliary models are passed as local paths, so nothing is downloaded at run time.
"""
from pathlib import Path
import os
import tempfile

import numpy as np

EMOTION = {
    "neutral": None,
    "excited": [0.75, 0.0, 0.0, 0.0, 0.0, 0.0, 0.45, 0.0],
    "calm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.85],
    "mystery": [0.0, 0.0, 0.0, 0.35, 0.0, 0.35, 0.0, 0.2],
}


class Adapter:
    def __init__(self, weights_dir, device, status):
        from indextts.infer_v2 import IndexTTS2

        d = Path(weights_dir)
        self.device = "cuda" if device == "cuda" else "cpu"
        aux = {
            "w2v_bert": str(d / "w2v-bert"),
            "semantic_codec": str(d / "maskgct" / "semantic_codec" / "model.safetensors"),
            "campplus": str(d / "campplus" / "campplus_cn_common.bin"),
            "bigvgan": str(d / "bigvgan"),
        }
        self.tts = IndexTTS2(cfg_path=str(d / "config.yaml"), model_dir=str(d), use_fp16=self.device == "cuda",
                             device="cuda:0" if self.device == "cuda" else "cpu", use_cuda_kernel=False,
                             use_deepspeed=False, use_torch_compile=False, use_qwen_emo=False, aux_paths=aux)
        self.tmp = Path(tempfile.gettempdir())

    def synth(self, text, *, voice=None, ref=None, ref_text=None, language=None, speed=1.0,
              emotion="neutral", exaggeration=0.5, seed=0):
        if not ref:
            raise RuntimeError("IndexTTS-2 needs a speaker reference: turn on Voice Clone and pick a voice profile.")
        import soundfile as sf
        out = self.tmp / f"vs-indextts-{os.getpid()}-{seed}.wav"
        vec = EMOTION.get(emotion)
        self.tts.infer(spk_audio_prompt=ref, text=text, output_path=str(out), emo_vector=vec,
                       emo_alpha=1.0, use_random=False, verbose=False)
        y, sr = sf.read(str(out), dtype="float32")
        try:
            out.unlink()
        except OSError:
            pass
        if y.ndim > 1:
            y = y.mean(axis=1)
        return y.astype("float32"), int(sr)
