"""Voice Studio isolated Chatterbox worker.

Runs inside its own virtual environment (installed on demand by Voice Studio with uv).
Protocol: one JSON object per line on stdin; one JSON reply per line on stdout.
Everything else that libraries print is redirected to stderr, so stdout stays clean.

The worker exits when stdin closes (parent quit or crashed) or when the parent PID
disappears, so it never outlives the app.
"""
import json
import os
import sys
import threading
import time
import traceback

PROTOCOL_VERSION = 1

# Keep the real stdout for protocol messages; route library prints to stderr.
_proto = os.fdopen(os.dup(1), "w", encoding="utf-8", buffering=1)
os.dup2(2, 1)
sys.stdout = sys.stderr
_lock = threading.Lock()


def send(obj):
    with _lock:
        _proto.write(json.dumps(obj) + "\n")
        _proto.flush()


def watch_parent():
    ppid = int(os.environ.get("VS_PARENT_PID", "0") or 0)
    if not ppid:
        return
    while True:
        time.sleep(2.0)
        try:
            if os.name == "nt":
                import ctypes
                k = ctypes.windll.kernel32
                h = k.OpenProcess(0x1000, False, ppid)  # PROCESS_QUERY_LIMITED_INFORMATION
                if not h:
                    os._exit(0)
                code = ctypes.c_ulong()
                k.GetExitCodeProcess(h, ctypes.byref(code))
                k.CloseHandle(h)
                if code.value != 259:  # STILL_ACTIVE
                    os._exit(0)
            else:
                os.kill(ppid, 0)
        except Exception:
            os._exit(0)


def pick_device(requested):
    import torch
    if requested == "cpu":
        return "cpu"
    if torch.cuda.is_available():
        return "cuda"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


def model_class(variant, ckpt_dir):
    """English (default), multilingual (23 languages) or Turbo (350M, faster)."""
    if variant == "mtl":
        from chatterbox import mtl_tts
        from chatterbox.models.tokenizers import tokenizer as tok
        from pathlib import Path
        original = tok.hf_hub_download

        def local_first(repo_id=None, filename=None, **kw):
            # The Chinese Cangjie table is fetched from the Hub by default; use the copy
            # shipped in the model folder so multilingual works offline.
            p = Path(ckpt_dir) / (filename or "")
            return str(p) if p.is_file() else original(repo_id=repo_id, filename=filename, **kw)

        tok.hf_hub_download = local_first
        return mtl_tts.ChatterboxMultilingualTTS
    if variant == "turbo":
        from chatterbox.tts_turbo import ChatterboxTurboTTS
        return ChatterboxTurboTTS
    from chatterbox.tts import ChatterboxTTS
    return ChatterboxTTS


def main():
    threading.Thread(target=watch_parent, daemon=True).start()
    model = None
    device = None
    cond_key = None
    builtin_conds = None
    variant = "en"
    send({"event": "ready", "protocol": PROTOCOL_VERSION, "pid": os.getpid()})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception as e:
            send({"ok": False, "error": f"bad request: {e}"})
            continue
        rid = req.get("id")
        cmd = req.get("cmd")
        try:
            if cmd == "ping":
                send({"id": rid, "ok": True})
            elif cmd == "info":
                import torch
                send({"id": rid, "ok": True, "torch": torch.__version__, "cuda": torch.cuda.is_available(),
                      "cuda_device": torch.cuda.get_device_name(0) if torch.cuda.is_available() else None,
                      "mps": bool(getattr(torch.backends, "mps", None) and torch.backends.mps.is_available())})
            elif cmd == "load":
                send({"event": "status", "message": "Starting Python and importing PyTorch (first start after boot can take a minute)…"})
                import torch
                send({"event": "status", "message": "Importing Chatterbox…"})
                variant = req.get("variant", "en")
                Cls = model_class(variant, req["ckpt_dir"])
                device = pick_device(req.get("device", "auto"))
                send({"event": "status", "message": f"Loading Chatterbox weights on {device.upper()}…"})
                t0 = time.time()
                try:
                    model = Cls.from_local(req["ckpt_dir"], device)
                except Exception:
                    if device == "cpu":
                        raise
                    # GPU path failed (driver, OOM, unsupported op): fall back to CPU.
                    traceback.print_exc()
                    device = "cpu"
                    model = Cls.from_local(req["ckpt_dir"], device)
                cond_key = None
                builtin_conds = model.conds
                send({"id": rid, "ok": True, "device": device, "sr": model.sr, "load_secs": time.time() - t0,
                      "torch": torch.__version__,
                      "gpu_name": torch.cuda.get_device_name(0) if device == "cuda" else None})
            elif cmd == "synth":
                if model is None:
                    raise RuntimeError("model not loaded")
                import random
                import numpy as np
                import soundfile as sf
                import torch
                ref = req.get("ref")
                ex = float(req.get("exaggeration", 0.5))
                if ref:
                    key = (ref, os.path.getmtime(ref), os.path.getsize(ref))
                    if key != cond_key:
                        model.prepare_conditionals(ref, exaggeration=ex)
                        cond_key = key
                elif cond_key is not None:
                    # Switch back to the built-in voice loaded with the model.
                    model.conds = builtin_conds
                    cond_key = None
                seed = int(req.get("seed", 0)) & 0xFFFFFFFF
                random.seed(seed)
                np.random.seed(seed)
                torch.manual_seed(seed)
                if device == "cuda":
                    torch.cuda.manual_seed_all(seed)
                    torch.cuda.reset_peak_memory_stats()
                t0 = time.time()
                kwargs = {"temperature": float(req.get("temperature", 0.8))}
                if variant != "turbo":  # Turbo has no exaggeration / CFG controls
                    kwargs["exaggeration"] = ex
                    kwargs["cfg_weight"] = float(req.get("cfg_weight", 0.5))
                if variant == "mtl":
                    kwargs["language_id"] = (req.get("language") or "en").lower()
                wav = model.generate(req["text"], **kwargs)
                audio = wav.squeeze(0).detach().cpu().numpy().astype("float32")
                sf.write(req["out"], audio, model.sr, subtype="FLOAT")
                peak = torch.cuda.max_memory_allocated() if device == "cuda" else None
                send({"id": rid, "ok": True, "out": req["out"], "secs": len(audio) / model.sr,
                      "gen_secs": time.time() - t0, "peak_vram": peak})
            elif cmd == "unload":
                model = None
                cond_key = None
                builtin_conds = None
                import gc
                gc.collect()
                try:
                    import torch
                    if torch.cuda.is_available():
                        torch.cuda.empty_cache()
                except Exception:
                    pass
                send({"id": rid, "ok": True})
            elif cmd == "quit":
                send({"id": rid, "ok": True})
                break
            else:
                send({"id": rid, "ok": False, "error": f"unknown command {cmd!r}"})
        except Exception as e:
            send({"id": rid, "ok": False, "error": f"{type(e).__name__}: {e}", "trace": traceback.format_exc()})
    os._exit(0)


if __name__ == "__main__":
    main()
