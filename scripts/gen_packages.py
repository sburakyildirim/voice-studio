"""Regenerate catalog/packages.json from upstream metadata (Hugging Face, PyPI, GitHub).

Every file gets its upstream SHA-256 so downloads are verified. Run with network access:
    python scripts/gen_packages.py > catalog/packages.json
Non-LFS files on Hugging Face (small JSON) have no published SHA-256; they are fetched once
here and hashed locally.
"""
import hashlib, json, sys, urllib.request

def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "voice-studio-catalog"})
    with urllib.request.urlopen(req) as r:
        return r.read()

def hf_tree(repo, rev, path=""):
    return json.loads(get(f"https://huggingface.co/api/models/{repo}/tree/{rev}/{path}"))

def hf_file(repo, rev, path, dest, entry=None):
    entry = entry or next(f for f in hf_tree(repo, rev, path.rsplit('/', 1)[0] if '/' in path else '') if f['path'] == path)
    url = f"https://huggingface.co/{repo}/resolve/{rev}/{path}"
    if entry.get('lfs'):
        sha, size = entry['lfs']['oid'], entry['lfs']['size']
    else:
        data = get(url); sha, size = hashlib.sha256(data).hexdigest(), len(data)
    return {"url": url, "sha256": sha, "size": size, "dest": dest}

def raw_file(url, dest):
    data = get(url)
    return {"url": url, "sha256": hashlib.sha256(data).hexdigest(), "size": len(data), "dest": dest}

KOKORO = ("onnx-community/Kokoro-82M-v1.0-ONNX", "1939ad2a8e416c0acfeecc08a694d14ef25f2231")
CHATTERBOX = ("ResembleAI/chatterbox", "5bb1f6ee58e50c3b8d408bc82a6d3740c2db6e18")
MISAKI = "fba1236595f2d2bf21d414ba6e57d25256afada3"

pk = []
onnx = {f['path']: f for f in hf_tree(*KOKORO, "onnx")}
for pid, fname, title, note in [
    ("kokoro-82m-fp32", "model.onnx", "Kokoro 82M v1.0 (fp32, recommended)", "Full quality. Measured ~3x faster than the 8-bit file on an x86 laptop CPU (ORT 1.24)."),
    ("kokoro-82m-q8", "model_quantized.onnx", "Kokoro 82M v1.0 (8-bit, small download)", "3.5x smaller download, slightly lower quality; measured ~3x slower than fp32 on x86 CPU."),
]:
    pk.append({"id": pid, "title": title, "kind": "model", "engine": "kokoro", "note": note,
               "license": "Apache-2.0", "files": [hf_file(*KOKORO, f"onnx/{fname}", "model.onnx", onnx[f"onnx/{fname}"])]})

voices = [f for f in hf_tree(*KOKORO, "voices") if f['path'].split('/')[-1][:2] in ("af", "am", "bf", "bm") and f['path'] != 'voices/af.bin']
pk.append({"id": "kokoro-voices-en", "title": "Kokoro English voices (US + UK, 28 voices)", "kind": "voices", "engine": "kokoro",
           "license": "Apache-2.0", "files": [hf_file(*KOKORO, v['path'], v['path'].split('/')[-1], v) for v in voices]})

pk.append({"id": "misaki-lexicon-en", "title": "misaki English pronunciation lexicon (US + UK)", "kind": "lexicon", "engine": "kokoro",
           "license": "Apache-2.0", "files": [raw_file(f"https://raw.githubusercontent.com/hexgrad/misaki/{MISAKI}/misaki/data/{n}.json", f"{n}.json")
                                               for n in ("us_gold", "us_silver", "gb_gold", "gb_silver")]})

cb = {f['path']: f for f in hf_tree(*CHATTERBOX)}
# No PyTorch 2.6 wheels exist for Intel macOS, so the weights are not offered there.
pk.append({"id": "chatterbox-en", "title": "Chatterbox (English, 0.5B) weights", "kind": "model", "engine": "chatterbox",
           "platforms": ["windows-x86_64", "windows-aarch64", "macos-aarch64", "linux-x86_64", "linux-aarch64"],
           "license": "MIT", "note": "Outputs carry Resemble AI's Perth imperceptible watermark.",
           "files": [hf_file(*CHATTERBOX, n, n, cb[n]) for n in ("tokenizer.json", "conds.pt", "ve.safetensors", "s3gen.safetensors", "t3_cfg.safetensors")]})

CB_PLATFORMS = ["windows-x86_64", "windows-aarch64", "macos-aarch64", "linux-x86_64", "linux-aarch64"]
mtl = ["ve.pt", "t3_mtl23ls_v2.safetensors", "s3gen.pt", "grapheme_mtl_merged_expanded_v1.json", "conds.pt", "Cangjie5_TC.json"]
pk.append({"id": "chatterbox-mtl", "title": "Chatterbox Multilingual (23 languages) weights", "kind": "model", "engine": "chatterbox",
           "platforms": CB_PLATFORMS, "license": "MIT", "note": "Uses the same Python runtime as Chatterbox. Output is watermarked (Perth).",
           "files": [hf_file(*CHATTERBOX, n, n, cb[n]) for n in mtl]})
TURBO = ("ResembleAI/chatterbox-turbo", "749d1c1a46eb10492095d68fbcf55691ccf137cd")
tb = {f['path']: f for f in hf_tree(*TURBO)}
turbo = ["ve.safetensors", "t3_turbo_v1.safetensors", "s3gen_meanflow.safetensors", "conds.pt", "vocab.json", "merges.txt",
         "tokenizer_config.json", "special_tokens_map.json", "added_tokens.json"]
pk.append({"id": "chatterbox-turbo", "title": "Chatterbox Turbo (English, 350M, faster) weights", "kind": "model", "engine": "chatterbox",
           "platforms": CB_PLATFORMS, "license": "MIT", "note": "Faster; no emotion-intensity control. Paralinguistic tags such as \\[laugh\\] can be written escaped.",
           "files": [hf_file(*TURBO, n, n, tb[n]) for n in turbo]})

def pypi_wheel(pkg, ver, fname):
    d = json.loads(get(f"https://pypi.org/pypi/{pkg}/{ver}/json"))
    u = next(u for u in d['urls'] if u['filename'] == fname)
    return {"url": u['url'], "sha256": u['digests']['sha256'], "size": u['size'], "dest": fname}

def gh_asset(repo, tag, name):
    d = json.loads(get(f"https://api.github.com/repos/{repo}/releases/tags/{tag}"))
    a = next(a for a in d['assets'] if a['name'] == name)
    return {"url": a['browser_download_url'], "sha256": a['digest'].split(':', 1)[1], "size": a['size'], "dest": name}

ort = [
    ("onnxruntime-win-x64-directml", "windows-x86_64", "ONNX Runtime 1.24.4 (DirectML GPU + CPU)",
     pypi_wheel("onnxruntime-directml", "1.24.4", "onnxruntime_directml-1.24.4-cp311-cp311-win_amd64.whl"),
     [("onnxruntime/capi/onnxruntime.dll", "onnxruntime.dll"), ("onnxruntime/capi/DirectML.dll", "DirectML.dll"),
      ("onnxruntime/capi/onnxruntime_providers_shared.dll", "onnxruntime_providers_shared.dll")]),
    ("onnxruntime-win-x64-cpu", "windows-x86_64", "ONNX Runtime 1.31.0 (CPU only)",
     pypi_wheel("onnxruntime", "1.31.0", "onnxruntime-1.31.0-cp311-cp311-win_amd64.whl"),
     [("onnxruntime/capi/onnxruntime.dll", "onnxruntime.dll"), ("onnxruntime/capi/onnxruntime_providers_shared.dll", "onnxruntime_providers_shared.dll")]),
    ("onnxruntime-macos-arm64", "macos-aarch64", "ONNX Runtime 1.31.0 (CPU + CoreML)",
     gh_asset("microsoft/onnxruntime", "v1.31.0", "onnxruntime-osx-arm64-1.31.0.tgz"),
     [("onnxruntime-osx-arm64-1.31.0/lib/libonnxruntime.1.31.0.dylib", "libonnxruntime.dylib")]),
    ("onnxruntime-macos-x64", "macos-x86_64", "ONNX Runtime 1.23.2 (CPU + CoreML; last Intel macOS build)",
     gh_asset("microsoft/onnxruntime", "v1.23.2", "onnxruntime-osx-x86_64-1.23.2.tgz"),
     [("onnxruntime-osx-x86_64-1.23.2/lib/libonnxruntime.1.23.2.dylib", "libonnxruntime.dylib")]),
    ("onnxruntime-linux-x64", "linux-x86_64", "ONNX Runtime 1.31.0 (CPU)",
     gh_asset("microsoft/onnxruntime", "v1.31.0", "onnxruntime-linux-x64-1.31.0.tgz"),
     [("onnxruntime-linux-x64-1.31.0/lib/libonnxruntime.so.1.31.0", "libonnxruntime.so")]),
    ("onnxruntime-linux-arm64", "linux-aarch64", "ONNX Runtime 1.31.0 (CPU)",
     gh_asset("microsoft/onnxruntime", "v1.31.0", "onnxruntime-linux-aarch64-1.31.0.tgz"),
     [("onnxruntime-linux-aarch64-1.31.0/lib/libonnxruntime.so.1.31.0", "libonnxruntime.so")]),
]
for pid, plat, title, f, members in ort:
    f["extract"] = [{"member": m, "to": t} for m, t in members]
    pk.append({"id": pid, "title": title, "kind": "runtime", "engine": "onnxruntime", "platforms": [plat],
               "license": "MIT", "files": [f]})

uv = [("windows-x86_64", "uv-x86_64-pc-windows-msvc.zip", "uv.exe"), ("macos-aarch64", "uv-aarch64-apple-darwin.tar.gz", "uv"),
      ("macos-x86_64", "uv-x86_64-apple-darwin.tar.gz", "uv"), ("linux-x86_64", "uv-x86_64-unknown-linux-gnu.tar.gz", "uv"),
      ("linux-aarch64", "uv-aarch64-unknown-linux-gnu.tar.gz", "uv")]
for plat, name, exe in uv:
    f = gh_asset("astral-sh/uv", "0.13.1", name)
    f["extract"] = [{"member_suffix": exe, "to": exe}]
    pk.append({"id": f"uv-{plat}", "title": "uv 0.13.1 (Python environment manager)", "kind": "tool", "engine": "python",
               "platforms": [plat], "license": "MIT OR Apache-2.0", "files": [f]})

json.dump({"schema_version": 1, "packages": pk}, sys.stdout, indent=1, ensure_ascii=False)
