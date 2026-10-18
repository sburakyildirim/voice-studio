"""Voice Studio generic engine worker.

Hosts one TTS model through a small adapter module (worker/adapters/<name>.py) inside that
model's own isolated virtual environment. Protocol: one JSON request per line on stdin, one
JSON reply per line on stdout; library output is redirected to stderr. The process exits when
stdin closes or the parent process disappears, so it never outlives Voice Studio.

Adapter contract (module-level class `Adapter`):
    Adapter(weights_dir: str, device: str, status: callable)   -> loads the model
    adapter.device: str                                         -> device actually used
    adapter.synth(text, *, voice, ref, ref_text, language, speed, emotion, exaggeration,
                  seed) -> (float32 numpy array, sample_rate)
"""
import importlib
import json
import os
import sys
import threading
import time
import traceback

PROTOCOL_VERSION = 1

_proto = os.fdopen(os.dup(1), "w", encoding="utf-8", buffering=1)
os.dup2(2, 1)
sys.stdout = sys.stderr
_lock = threading.Lock()
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def send(obj):
    with _lock:
        _proto.write(json.dumps(obj) + "\n")
        _proto.flush()


def status(message):
    send({"event": "status", "message": message})


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
                h = k.OpenProcess(0x1000, False, ppid)
                if not h:
                    os._exit(0)
                code = ctypes.c_ulong()
                k.GetExitCodeProcess(h, ctypes.byref(code))
                k.CloseHandle(h)
                if code.value != 259:
                    os._exit(0)
            else:
                os.kill(ppid, 0)
        except Exception:
            os._exit(0)


def pick_device(requested):
    try:
        import torch
    except ImportError:
        return "cpu"
    if requested == "cpu":
        return "cpu"
    if torch.cuda.is_available():
        return "cuda"
    if getattr(torch.backends, "mps", None) and torch.backends.mps.is_available():
        return "mps"
    return "cpu"


def seed_everything(seed):
    import random
    random.seed(seed)
    try:
        import numpy as np
        np.random.seed(seed)
    except Exception:
        pass
    try:
        import torch
        torch.manual_seed(seed)
        if torch.cuda.is_available():
            torch.cuda.manual_seed_all(seed)
    except Exception:
        pass


def write_wav(path, audio, sr):
    import numpy as np
    import wave
    a = np.asarray(audio, dtype="float32").reshape(-1)
    pcm = (np.clip(a, -1.0, 1.0) * 32767.0).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(int(sr))
        w.writeframes(pcm.tobytes())
    return len(a) / float(sr)


def main():
    threading.Thread(target=watch_parent, daemon=True).start()
    adapter = None
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
            elif cmd == "load":
                name = req["adapter"]
                status(f"Importing the {name} engine…")
                mod = importlib.import_module(f"adapters.{name}")
                device = pick_device(req.get("device", "auto"))
                status(f"Loading {name} weights on {device.upper()}…")
                t0 = time.time()
                try:
                    adapter = mod.Adapter(req["weights_dir"], device, status)
                except Exception:
                    if device == "cpu":
                        raise
                    traceback.print_exc()
                    status("GPU load failed; retrying on CPU…")
                    adapter = mod.Adapter(req["weights_dir"], "cpu", status)
                gpu_name = None
                if getattr(adapter, "device", "cpu") == "cuda":
                    import torch
                    gpu_name = torch.cuda.get_device_name(0)
                send({"id": rid, "ok": True, "device": getattr(adapter, "device", device),
                      "load_secs": time.time() - t0, "gpu_name": gpu_name})
            elif cmd == "synth":
                if adapter is None:
                    raise RuntimeError("model not loaded")
                seed_everything(int(req.get("seed", 0)) & 0xFFFFFFFF)
                peak = None
                try:
                    import torch
                    if torch.cuda.is_available():
                        torch.cuda.reset_peak_memory_stats()
                except Exception:
                    pass
                t0 = time.time()
                audio, sr = adapter.synth(
                    req["text"],
                    voice=req.get("voice"),
                    ref=req.get("ref"),
                    ref_text=req.get("ref_text"),
                    language=req.get("language"),
                    speed=float(req.get("speed", 1.0)),
                    emotion=req.get("emotion", "neutral"),
                    exaggeration=float(req.get("exaggeration", 0.5)),
                    seed=int(req.get("seed", 0)),
                )
                secs = write_wav(req["out"], audio, sr)
                try:
                    import torch
                    if torch.cuda.is_available():
                        peak = torch.cuda.max_memory_allocated()
                except Exception:
                    pass
                send({"id": rid, "ok": True, "out": req["out"], "secs": secs,
                      "gen_secs": time.time() - t0, "peak_vram": peak})
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
