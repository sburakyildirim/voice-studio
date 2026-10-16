"""Objective checks for rendered Voice Studio projects (development / QA tool).

Run with the Chatterbox runtime's Python (it already has torch, librosa, transformers):

    <runtime>/venv/Scripts/python.exe scripts/verify_audio.py <project_dir> [--ref reference.wav] [--baseline other_project]

Reports, per project:
  * intelligibility: Whisper (openai/whisper-base.en) transcript vs. the script's words → WER
  * speaker similarity: cosine similarity of Chatterbox's VoiceEncoder embeddings
      - final render vs. the reference clip (cloning)
      - each segment vs. the mean of all segments (voice stability across segments)
      - optional baseline project (e.g. the same text with the built-in voice) vs. reference,
        to show the similarity a *non*-cloned voice gets
  * basic signal stats: duration, peak, clipped samples, longest silence

Whisper downloads ~290 MB into the Hugging Face cache on first use. Nothing here is part of
the shipped app; it only measures what the app produced.
"""
import argparse
import json
import re
import sys
from pathlib import Path

import numpy as np
import librosa


def words(s):
    s = s.lower().replace("’", "'")
    s = re.sub(r"[^a-z0-9' ]+", " ", s)
    return [w for w in s.split() if w]


def wer(ref, hyp):
    r, h = words(ref), words(hyp)
    d = np.zeros((len(r) + 1, len(h) + 1), dtype=int)
    d[:, 0] = range(len(r) + 1)
    d[0, :] = range(len(h) + 1)
    for i in range(1, len(r) + 1):
        for j in range(1, len(h) + 1):
            d[i, j] = min(d[i - 1, j] + 1, d[i, j - 1] + 1, d[i - 1, j - 1] + (r[i - 1] != h[j - 1]))
    return d[len(r), len(h)] / max(1, len(r))


_asr = None
_ve = None


def asr(path):
    global _asr
    if _asr is None:
        from transformers import pipeline
        import torch
        _asr = pipeline("automatic-speech-recognition", model="openai/whisper-base.en",
                        device=0 if torch.cuda.is_available() else -1)
    y, _ = librosa.load(str(path), sr=16000)
    out = _asr({"raw": y, "sampling_rate": 16000}, return_timestamps=True)
    return out["text"].strip()


def embed(path):
    global _ve
    import torch
    if _ve is None:
        from chatterbox.models.voice_encoder import VoiceEncoder
        from safetensors.torch import load_file
        import os
        ckpt = Path(os.environ.get("VS_CHATTERBOX_DIR", Path(os.environ["LOCALAPPDATA"]) / "VoiceStudio" / "models" / "chatterbox-en"))
        _ve = VoiceEncoder()
        _ve.load_state_dict(load_file(str(ckpt / "ve.safetensors")))
        _ve.eval()
    y, _ = librosa.load(str(path), sr=16000)
    with torch.inference_mode():
        e = _ve.embeds_from_wavs([y], sample_rate=16000)
    e = np.asarray(e).mean(axis=0)
    return e / np.linalg.norm(e)


def cos(a, b):
    return float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b)))


def signal_stats(path):
    y, sr = librosa.load(str(path), sr=None)
    frame = int(sr * 0.02)
    rms = np.array([np.sqrt(np.mean(y[i:i + frame] ** 2)) for i in range(0, len(y) - frame, frame)])
    silent = rms < 10 ** (-50 / 20)
    longest, cur = 0, 0
    for s in silent:
        cur = cur + 1 if s else 0
        longest = max(longest, cur)
    return {"duration_s": round(len(y) / sr, 2), "peak_dbfs": round(20 * np.log10(np.max(np.abs(y)) + 1e-9), 2),
            "clipped_samples": int(np.sum(np.abs(y) >= 0.999)), "longest_silence_s": round(longest * 0.02, 2)}


def emotion_stats(proj, meta):
    """Per-emotion delivery measurements over the rendered segments: speaking rate
    (words/s), RMS level (dBFS, after the per-style gain) and pitch (median and spread of
    f0 over voiced frames, librosa.yin). Shows whether markup measurably changes delivery."""
    groups = {}
    for s in meta["segments"]:
        y, sr = librosa.load(str(proj / s["file"]), sr=16000)
        if len(y) < 1600:
            continue
        f0 = librosa.yin(y, fmin=65, fmax=400, sr=sr, frame_length=1024)
        rms = librosa.feature.rms(y=y, frame_length=1024, hop_length=256)[0]
        voiced = f0[(f0 > 66) & (f0 < 399)]
        g = groups.setdefault(s["emotion"], {"words": 0, "secs": 0.0, "rms": [], "f0_med": [], "f0_std": []})
        g["words"] += len(s["text"].split())
        g["secs"] += len(y) / sr
        g["rms"].append(20 * np.log10(np.sqrt(np.mean(y ** 2)) + 1e-9))
        if len(voiced) > 10:
            g["f0_med"].append(float(np.median(voiced)))
            g["f0_std"].append(float(np.std(voiced)))
    out = {}
    for emo, g in groups.items():
        out[emo] = {
            "segments": len(g["rms"]),
            "words_per_sec": round(g["words"] / max(g["secs"], 1e-6), 2),
            "rms_dbfs": round(float(np.mean(g["rms"])), 1),
            "f0_median_hz": round(float(np.mean(g["f0_med"])), 1) if g["f0_med"] else None,
            "f0_spread_hz": round(float(np.mean(g["f0_std"])), 1) if g["f0_std"] else None,
        }
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("project")
    ap.add_argument("--ref")
    ap.add_argument("--baseline")
    ap.add_argument("--no-asr", action="store_true")
    ap.add_argument("--emotion", action="store_true", help="per-emotion rate/energy/pitch analysis")
    a = ap.parse_args()
    proj = Path(a.project)
    meta = json.loads((proj / "project.json").read_text(encoding="utf-8"))
    final = proj / meta["final_wav"]
    expected = " ".join(s["text"] for s in meta["segments"])
    report = {"project": meta["title"], "engine": meta["engine"], "voice": meta.get("profile_name") or meta["voice"],
              "signal": signal_stats(final)}
    if not a.no_asr:
        hyp = asr(final)
        report["asr_transcript"] = hyp
        report["wer"] = round(wer(expected, hyp), 3)
    ref = Path(a.ref) if a.ref else (proj / "reference.wav" if (proj / "reference.wav").exists() else None)
    seg_embs = [embed(proj / s["file"]) for s in meta["segments"] if s["duration_secs"] >= 1.0]
    if seg_embs:
        centroid = np.mean(seg_embs, axis=0)
        sims = [cos(e, centroid) for e in seg_embs]
        report["segment_stability"] = {"min": round(min(sims), 3), "mean": round(float(np.mean(sims)), 3), "n": len(sims)}
    if ref:
        r = embed(ref)
        report["similarity_to_reference"] = round(cos(embed(final), r), 3)
        if a.baseline:
            bmeta = json.loads((Path(a.baseline) / "project.json").read_text(encoding="utf-8"))
            report["baseline_similarity_to_reference"] = round(cos(embed(Path(a.baseline) / bmeta["final_wav"]), r), 3)
            report["baseline"] = bmeta.get("profile_name") or bmeta["voice"]
    if a.emotion:
        report["by_emotion"] = emotion_stats(proj, meta)
    print(json.dumps(report, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    sys.exit(main())
