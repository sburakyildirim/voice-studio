// Options dialog: Models (download manager + evaluated catalog), Voices (reference import,
// trim, preview, quality check, consent), Settings, Diagnostics, About.
import { open as openDialog, ask } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { api, esc, fmtBytes, fmtTime, PackageStatus, ReferencePreview, Settings } from "./api";
import { toast, promptText } from "./toast";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const panel = (name: string) => document.querySelector<HTMLElement>(`[data-panel="${name}"]`)!;

const progress: Record<string, { done: number; total: number; bps: number; file: string }> = {};
let runtimeLog: string[] = [];
let onChange: () => void = () => {};

export function initOptions(changed: () => void) {
  onChange = changed;
  const dlg = $<HTMLDialogElement>("options");
  $("opt-close").onclick = () => dlg.close();
  dlg.addEventListener("close", () => {
    stopRefPreview();
    onChange();
  });
  document.querySelectorAll<HTMLButtonElement>(".tabs button").forEach((b) => {
    b.onclick = () => showTab(b.dataset.tab!);
  });
  listen<any>("download-progress", (e) => {
    progress[e.payload.id] = e.payload;
    updateProgressBars();
  });
  listen<any>("download-done", (e) => {
    delete progress[e.payload.id];
    if (e.payload.ok) toast(`Installed ${e.payload.id}`, "ok");
    else if (e.payload.cancelled) toast(`Download paused: ${e.payload.id} (resume any time)`);
    else toast(`Download failed (${e.payload.id}): ${e.payload.error}`, "error");
    if (dlg.open) renderModels();
    onChange();
  });
  listen<string>("runtime-log", (e) => {
    runtimeLog.push(e.payload);
    if (runtimeLog.length > 400) runtimeLog = runtimeLog.slice(-400);
    const el = document.getElementById("rt-log");
    if (el) {
      el.textContent = runtimeLog.join("\n");
      el.scrollTop = el.scrollHeight;
    }
  });
}

export function openOptions(tab = "models") {
  const dlg = $<HTMLDialogElement>("options");
  if (!dlg.open) dlg.showModal();
  showTab(tab);
}

function showTab(tab: string) {
  document.querySelectorAll<HTMLButtonElement>(".tabs button").forEach((b) => b.classList.toggle("on", b.dataset.tab === tab));
  document.querySelectorAll<HTMLElement>(".panel").forEach((p) => p.classList.toggle("on", p.dataset.panel === tab));
  if (tab === "models") renderModels();
  if (tab === "voices") renderVoices();
  if (tab === "settings") renderSettings();
  if (tab === "diag") renderDiag();
  if (tab === "about") renderAbout();
}

function updateProgressBars() {
  for (const [id, p] of Object.entries(progress)) {
    const el = document.querySelector<HTMLElement>(`[data-progress="${CSS.escape(id)}"]`);
    if (!el) continue;
    el.hidden = false;
    const pct = p.total ? (p.done / p.total) * 100 : 0;
    const eta = p.bps > 0 ? (p.total - p.done) / p.bps : 0;
    el.querySelector<HTMLElement>(".bar")!.style.width = `${pct.toFixed(1)}%`;
    el.querySelector<HTMLElement>(".txt")!.textContent = `${pct.toFixed(1)}% · ${fmtBytes(p.done)} / ${fmtBytes(p.total)} · ${(p.bps / 1e6).toFixed(1)} MB/s · ETA ${fmtTime(eta)} · ${p.file}`;
  }
}

function pkgRow(p: PackageStatus, extra = ""): string {
  const busy = !!progress[p.id];
  const status = p.installed
    ? `<span class="badge ok">installed · ${fmtBytes(p.disk_size)}</span>`
    : p.partial_bytes > 0
      ? `<span class="badge approximate">partial ${fmtBytes(p.partial_bytes)} — resumable</span>`
      : `<span class="badge">not installed</span>`;
  const acts = p.installed
    ? `<button data-act="verify" data-id="${p.id}">Verify</button><button data-act="remove" data-id="${p.id}" class="danger">Remove</button>`
    : busy
      ? `<button data-act="cancel" data-id="${p.id}">Pause</button>`
      : `<button data-act="install" data-id="${p.id}" class="primary">${p.partial_bytes ? "Resume" : "Download"} ${fmtBytes(p.download_size)}</button><button data-act="import" data-id="${p.id}" title="Use files you already downloaded (matched by SHA-256)">Import…</button>`;
  return `<div class="pkg"><div><div class="name">${esc(p.title)} ${status}</div>
    <div class="sub">${esc(p.id)} · license ${esc(p.license)}${p.note ? " · " + esc(p.note) : ""}${extra}</div></div>
    <div class="acts">${acts}</div>
    <div class="mini-progress" data-progress="${p.id}" ${busy ? "" : "hidden"}><div class="progress"><div class="bar" style="height:100%;background:var(--accent);width:0"></div></div><div class="sub txt"></div></div></div>`;
}

async function renderModels() {
  const el = panel("models");
  let cat: any, rt: any;
  try {
    [cat, rt] = await Promise.all([api.catalog(), api.runtimeStatus()]);
  } catch (e) {
    el.innerHTML = `<p>${esc(e)}</p>`;
    return;
  }
  const pk: PackageStatus[] = cat.packages;
  const by = (f: (p: PackageStatus) => boolean) => pk.filter(f).map((p) => pkgRow(p)).join("");
  const s = await api.getSettings();
  const bench: any[] = cat.benchmarks || [];
  const rtBusy = !!progress["chatterbox-runtime"] || rt.installing;
  el.innerHTML = `
    <div class="box"><h4>Storage</h4>
      <div class="kv"><div>Model folder</div><div><code>${esc(cat.models_dir)}</code></div>
      <div>Free space</div><div>${cat.free_space != null ? fmtBytes(cat.free_space) : "unknown"}</div></div>
      <div class="row" style="margin-top:8px"><button id="m-change">Change model folder…</button><button id="m-open">Open folder</button>
      <span class="hint">Models live outside the app install, so they survive updates. Everything works offline once downloaded.</span></div></div>

    <div class="box"><h4>Kokoro 82M — native ONNX (fast English TTS, no cloning)</h4>
      <div class="hint">Required: one model variant, the voices, the lexicon and an ONNX Runtime for this platform. Active variant: <b>${esc(s.kokoro_variant)}</b> (change in Settings).</div>
      ${by((p) => p.engine === "kokoro")}
      ${by((p) => p.engine === "onnxruntime")}
    </div>

    <div class="box"><h4>Chatterbox — voice cloning + emotion intensity (optional, isolated Python runtime)</h4>
      ${by((p) => p.engine === "chatterbox")}
      <div class="pkg"><div><div class="name">Chatterbox Python runtime ${rt.installed ? `<span class="badge ok">installed · ${fmtBytes(rt.disk_size)}</span>` : `<span class="badge">not installed</span>`}</div>
        <div class="sub">${rt.installed ? `Python ${esc(rt.installed.python)} · torch ${esc(rt.installed.torch)} · ${rt.installed.cuda ? "CUDA: " + esc(rt.installed.cuda_device) : rt.installed.mps ? "Apple MPS" : "CPU only"} · chatterbox-tts ${esc(rt.installed.chatterbox)}` : `Installs Python 3.11 + PyTorch 2.6 (${rt.nvidia ? "CUDA 12.4 build — NVIDIA GPU detected" : "CPU build — no NVIDIA GPU detected"}) + chatterbox-tts into <code>${esc(rt.dir)}</code>. About ${rt.estimated_download_gb} GB download. Uses uv (${rt.uv ? "found" : "downloaded automatically"}). Nothing touches your system Python.`}</div></div>
        <div class="acts">${rt.unsupported ? `<span class="badge unsupported" title="${esc(rt.unsupported)}">not supported on this Mac</span>` : rt.installed ? `<button id="rt-remove" class="danger">Remove</button>` : rtBusy ? `<button id="rt-cancel">Cancel</button>` : `<button id="rt-install" class="primary">Install runtime</button>`}</div></div>
      <pre class="log" id="rt-log" ${runtimeLog.length || rtBusy ? "" : "hidden"}>${esc(runtimeLog.join("\n"))}</pre>
    </div>

    <div class="box"><h4>Tools</h4>${by((p) => p.engine === "python")}</div>

    <div class="box"><h4>Evaluated models (${cat.models.models.length})</h4>
      <p class="hint">${esc(cat.models.disclaimer)} Evaluated ${esc(cat.models.evaluated_on)}.</p>
      <div style="overflow:auto"><table class="catalog"><thead><tr><th>Model</th><th>Status</th><th>Cloning</th><th>Emotion / style</th><th>Languages</th><th>Size</th><th>Measured here</th><th>License · commercial use</th><th>Hardware / notes</th></tr></thead><tbody>
      ${cat.models.models.map((m: any) => {
        const b = bench.filter((x) => (m.engine && x.engine === m.engine)).slice(-2);
        const meas = b.length ? b.map((x) => `${esc(x.device)}: RTF ${Number(x.warm_rtf).toFixed(2)}, RAM ${Math.round(x.peak_rss_mb || 0)} MB${x.peak_vram_mb_total_gpu ? `, GPU mem ${Math.round(x.peak_vram_mb_total_gpu)} MB (total)` : ""}`).join("<br>") : "not measured";
        return `<tr><td><b>${esc(m.name)}</b><br><span class="hint">${esc(m.vendor)} · <a href="#" data-src="${esc(m.source)}" title="${esc(m.source)}">source</a></span></td>
          <td class="st-${m.status}">${m.status === "integrated" ? "Integrated" : "Evaluated —<br>not integrated (unsupported)"}</td>
          <td>${esc(m.cloning)}</td><td>${esc(m.emotion)}${m.style_transfer ? `<br><span class="hint">style: ${esc(m.style_transfer)}</span>` : ""}</td>
          <td>${esc((m.languages || []).join(", "))}</td><td>${esc(m.params_reported || "")}</td><td>${meas}</td>
          <td>${esc(m.license)}<br><span class="hint">${esc(m.commercial_use)}</span></td>
          <td class="hint">${esc(m.hardware || m.not_integrated_reason || "")}${m.emotion_note ? "<br>" + esc(m.emotion_note) : ""}</td></tr>`;
      }).join("")}
      </tbody></table></div>
      <p class="hint">Naturalness is not scored (no listening test was run). Run <code>vscli bench</code> to add measurements for your machine.</p>
    </div>`;
  el.querySelectorAll<HTMLButtonElement>("button[data-act]").forEach((b) => {
    b.onclick = async () => {
      const id = b.dataset.id!;
      try {
        if (b.dataset.act === "install") {
          await api.installPackage(id);
          progress[id] = { done: 0, total: 1, bps: 0, file: "starting…" };
        } else if (b.dataset.act === "cancel") await api.cancelDownload(id);
        else if (b.dataset.act === "remove") {
          if (!(await ask(`Remove ${id}? You can download it again later.`, { kind: "warning" }))) return;
          await api.removePackage(id);
        } else if (b.dataset.act === "verify") {
          b.disabled = true;
          b.textContent = "Verifying…";
          const bad = await api.verifyPackage(id);
          toast(bad.length ? `Corrupt files in ${id}: ${bad.join(", ")} — remove and download again.` : `${id}: all files verified (SHA-256)`, bad.length ? "error" : "ok");
        } else if (b.dataset.act === "import") {
          const dir = await openDialog({ directory: true, title: `Folder containing files for ${id}` });
          if (!dir) return;
          const n = await api.importPackage(id, dir as string);
          toast(`Imported ${n} file(s) for ${id}`, "ok");
        }
      } catch (e) {
        toast(String(e), "error");
      }
      renderModels();
      onChange();
    };
  });
  el.querySelectorAll<HTMLAnchorElement>("a[data-src]").forEach((a) => (a.onclick = (e) => { e.preventDefault(); navigator.clipboard?.writeText(a.dataset.src!); toast(`Source copied: ${a.dataset.src}`); }));
  $("m-open").onclick = () => api.openFolder("models");
  $("m-change").onclick = async () => {
    const dir = await openDialog({ directory: true, title: "New model folder" });
    if (!dir) return;
    const move = await ask("Move the models you already downloaded to the new folder?", { title: "Model folder", kind: "info" });
    try {
      await api.moveModelsDir(dir as string, move);
      toast("Model folder changed", "ok");
    } catch (e) {
      toast(String(e), "error");
    }
    renderModels();
    onChange();
  };
  document.getElementById("rt-install")?.addEventListener("click", async () => {
    runtimeLog = ["Starting runtime installation…"];
    try {
      await api.installRuntime();
    } catch (e) {
      toast(String(e), "error");
    }
    renderModels();
  });
  document.getElementById("rt-cancel")?.addEventListener("click", () => api.cancelDownload("chatterbox-runtime"));
  document.getElementById("rt-remove")?.addEventListener("click", async () => {
    if (!(await ask("Remove the Chatterbox Python runtime (~5 GB)?", { kind: "warning" }))) return;
    try {
      await api.removeRuntime();
    } catch (e) {
      toast(String(e), "error");
    }
    renderModels();
    onChange();
  });
  updateProgressBars();
}

// ---------------------------------------------------------------- Voices

let ref: { path: string; pv: ReferencePreview; start: number; end: number } | null = null;
const refAudio = new Audio();
function stopRefPreview() {
  refAudio.pause();
}

async function renderVoices() {
  const el = panel("voices");
  const [v, info] = await Promise.all([api.voices(), api.appInfo()]);
  el.innerHTML = `
    <div class="box"><h4>Voice profiles (for Voice Clone)</h4>
      ${v.profiles.length ? `<table><thead><tr><th>Name</th><th>Length</th><th>Quality</th><th>Source</th><th>Consent</th><th></th></tr></thead><tbody>
        ${v.profiles.map((p) => `<tr><td><b>${esc(p.name)}</b></td><td>${p.duration_secs.toFixed(1)} s</td>
          <td><span class="badge ${p.quality.verdict === "good" ? "ok" : p.quality.verdict === "usable" ? "approximate" : "unsupported"}">${p.quality.verdict}</span> SNR≈${p.quality.snr_estimate_db.toFixed(0)} dB</td>
          <td class="hint">${esc(p.source_file)}<br>${new Date(p.created_at).toLocaleDateString()}</td><td>${p.consent.acknowledged ? "✓ confirmed" : "—"}</td>
          <td><button data-pr="rename" data-id="${p.id}">Rename</button> <button data-pr="delete" data-id="${p.id}" class="danger">Delete</button></td></tr>`).join("")}
      </tbody></table>` : `<p class="hint">No profiles yet.</p>`}
    </div>
    <div class="box"><h4>New profile from a reference recording</h4>
      <p class="hint">Use 30–60 seconds of clean speech from one speaker (WAV, MP3, FLAC, OGG or M4A). No music, no background noise, no other voices. Voice similarity is never guaranteed.</p>
      <div class="row"><button id="ref-pick" class="primary">Choose audio file…</button><span id="ref-name" class="hint"></span></div>
      <div id="ref-edit" hidden>
        <canvas id="ref-wave" class="wave" width="1000" height="120" aria-label="Waveform: drag to select the region to keep"></canvas>
        <div class="row" style="margin-top:8px">
          <label class="field"><span>Start (s)</span><input type="number" id="ref-start" step="0.1" min="0" /></label>
          <label class="field"><span>End (s)</span><input type="number" id="ref-end" step="0.1" min="0" /></label>
          <span id="ref-len" class="hint"></span>
          <button id="ref-play">▶ Preview selection</button>
          <button id="ref-check">Check quality</button>
        </div>
        <div id="ref-quality"></div>
        <label class="field"><span>Profile name</span><input type="text" id="ref-label" placeholder="e.g. My narration voice" /></label>
        <label class="consent"><input type="checkbox" id="ref-consent" /><span><b>Rights & consent.</b> ${esc(info.consent_text)}</span></label>
        <button id="ref-save" class="primary" disabled>Save profile</button>
      </div>
    </div>
    <div class="box"><h4>Built-in Kokoro voices (${v.kokoro.length})</h4>
      <p class="hint">${v.kokoro.map((k) => `${esc(k.name)} (${esc(k.accent)} ${esc(k.gender)})`).join(" · ") || "Install the Kokoro voices package in Models."}</p></div>`;
  el.querySelectorAll<HTMLButtonElement>("button[data-pr]").forEach((b) => {
    b.onclick = async () => {
      const id = b.dataset.id!;
      try {
        if (b.dataset.pr === "delete") {
          if (!(await ask("Delete this voice profile and its reference audio?", { kind: "warning" }))) return;
          await api.profileDelete(id);
        } else {
          const name = await promptText("Rename voice profile", v.profiles.find((x) => x.id === id)?.name ?? "");
          if (name) await api.profileRename(id, name);
        }
      } catch (e) {
        toast(String(e), "error");
      }
      renderVoices();
      onChange();
    };
  });
  $("ref-pick").onclick = pickReference;
  if (ref) showRef();
}

async function pickReference() {
  const path = await openDialog({ multiple: false, filters: [{ name: "Audio", extensions: ["wav", "mp3", "flac", "ogg", "m4a"] }] });
  if (!path) return;
  $("ref-name").textContent = "Analysing…";
  try {
    const pv = await api.referenceLoad(path as string);
    ref = { path: path as string, pv, start: pv.suggested_start, end: pv.suggested_end };
    showRef();
  } catch (e) {
    $("ref-name").textContent = "";
    toast(`Cannot use this file: ${e}`, "error");
  }
}

function showRef() {
  if (!ref) return;
  const r = ref;
  $("ref-edit").hidden = false;
  $("ref-name").textContent = `${r.path.split(/[\\/]/).pop()} · ${r.pv.duration_secs.toFixed(1)} s · ${r.pv.source_sample_rate} Hz`;
  const s = $<HTMLInputElement>("ref-start");
  const e = $<HTMLInputElement>("ref-end");
  s.value = r.start.toFixed(1);
  e.value = r.end.toFixed(1);
  s.max = e.max = String(r.pv.duration_secs);
  const upd = () => {
    r.start = Math.max(0, Math.min(Number(s.value), r.pv.duration_secs));
    r.end = Math.max(r.start, Math.min(Number(e.value), r.pv.duration_secs));
    drawWave();
    const len = r.end - r.start;
    $("ref-len").textContent = `${len.toFixed(1)} s selected ${len < 10 ? "· too short" : len < 30 ? "· 30–60 s recommended" : len > 60 ? "· longer than needed" : "· good length"}`;
    validate();
  };
  s.oninput = upd;
  e.oninput = upd;
  const cv = $<HTMLCanvasElement>("ref-wave");
  let dragStart: number | null = null;
  const tAt = (ev: MouseEvent) => ((ev.offsetX / cv.clientWidth) * r.pv.duration_secs);
  cv.onmousedown = (ev) => (dragStart = tAt(ev));
  cv.onmousemove = (ev) => {
    if (dragStart == null) return;
    const t = tAt(ev);
    s.value = Math.min(dragStart, t).toFixed(1);
    e.value = Math.max(dragStart, t).toFixed(1);
    upd();
  };
  window.onmouseup = () => (dragStart = null);
  $("ref-play").onclick = async () => {
    try {
      const c = await api.referenceClip(r.start, r.end);
      refAudio.src = `data:audio/wav;base64,${c.wav_b64}`;
      refAudio.play();
      showQuality(c.quality);
    } catch (err) {
      toast(String(err), "error");
    }
  };
  $("ref-check").onclick = async () => showQuality((await api.referenceClip(r.start, r.end)).quality);
  $<HTMLInputElement>("ref-consent").onchange = validate;
  $<HTMLInputElement>("ref-label").oninput = validate;
  $("ref-save").onclick = async () => {
    try {
      const p = await api.profileSave($<HTMLInputElement>("ref-label").value, r.start, r.end, $<HTMLInputElement>("ref-consent").checked);
      toast(`Saved voice profile "${p.name}" (${p.quality.verdict})`, "ok");
      ref = null;
      renderVoices();
      onChange();
    } catch (err) {
      toast(String(err), "error");
    }
  };
  showQuality(r.pv.quality);
  upd();
}

function validate() {
  if (!ref) return;
  const ok = $<HTMLInputElement>("ref-consent").checked && $<HTMLInputElement>("ref-label").value.trim().length > 0 && ref.end - ref.start >= 3;
  $<HTMLButtonElement>("ref-save").disabled = !ok;
}

function showQuality(q: any) {
  $("ref-quality").innerHTML = `<div class="row"><span class="badge ${q.verdict === "good" ? "ok" : q.verdict === "usable" ? "approximate" : "unsupported"}">quality: ${q.verdict}</span>
    <span class="hint">SNR≈${q.snr_estimate_db.toFixed(0)} dB · peak ${q.peak_dbfs.toFixed(1)} dBFS · loudness ${q.loudness_lufs != null ? q.loudness_lufs.toFixed(1) + " LUFS" : "—"} · silence ${(q.silence_ratio * 100).toFixed(0)}% · clipping ${(q.clipped_ratio * 100).toFixed(2)}%</span></div>
    ${q.issues.length ? `<ul class="issues">${q.issues.map((i: string) => `<li>${esc(i)}</li>`).join("")}</ul>` : ""}`;
}

function drawWave() {
  if (!ref) return;
  const cv = $<HTMLCanvasElement>("ref-wave");
  const g = cv.getContext("2d")!;
  const { peaks, duration_secs } = ref.pv;
  g.clearRect(0, 0, cv.width, cv.height);
  const sx = (ref.start / duration_secs) * cv.width;
  const ex = (ref.end / duration_secs) * cv.width;
  g.fillStyle = "rgba(126,240,180,.12)";
  g.fillRect(sx, 0, ex - sx, cv.height);
  const mid = cv.height / 2;
  peaks.forEach(([mn, mx], i) => {
    const x = (i / peaks.length) * cv.width;
    const inSel = x >= sx && x <= ex;
    g.fillStyle = inSel ? "#7ef0b4" : "#4a5070";
    g.fillRect(x, mid - mx * mid, Math.max(1, cv.width / peaks.length - 0.5), (mx - mn) * mid || 1);
  });
  g.fillStyle = "#ffc45c";
  g.fillRect(sx, 0, 2, cv.height);
  g.fillRect(ex - 2, 0, 2, cv.height);
}

// ---------------------------------------------------------------- Settings

async function renderSettings() {
  const el = panel("settings");
  const s = await api.getSettings();
  const st = await api.engineStatus();
  const cat = await api.catalog();
  const variants = (cat.packages as PackageStatus[]).filter((p) => p.id.startsWith("kokoro-82m"));
  el.innerHTML = `
    <div class="box"><h4>Output</h4>
      <div class="kv"><div>Output / projects folder</div><div><code>${esc(s.output_dir)}</code> <button id="s-out">Change…</button> <button id="s-open-out">Open</button></div>
      <div>MP3 export</div><div><label class="toggle ${st.mp3 ? "" : "disabled"}"><input type="checkbox" id="s-mp3" ${s.export_mp3 && st.mp3 ? "checked" : ""} ${st.mp3 ? "" : "disabled"} /><span class="sw"></span>${st.mp3 ? "Also write final.mp3 (192 kbps, via FFmpeg/LAME)" : "Unavailable — FFmpeg with libmp3lame not found. WAV is always written."}</label></div>
      <div>FFmpeg path (optional)</div><div><input type="text" id="s-ff" value="${esc(s.ffmpeg_path ?? "")}" placeholder="auto-detect from PATH" style="width:420px" /></div>
      <div>Loudness target</div><div><input type="number" id="s-lufs" value="${s.loudness_target_lufs}" step="0.5" min="-30" max="-9" /> LUFS (−16 suits Shorts/podcasts, −23 broadcast), peak ceiling ${s.peak_ceiling_db} dBFS</div></div></div>
    <div class="box"><h4>Performance</h4>
      <div class="kv"><div>Device</div><div><select id="s-dev">
        <option value="auto">Auto (Kokoro: CPU · Chatterbox: CUDA/MPS if available, else CPU)</option>
        <option value="cpu">CPU only</option>
        <option value="gpu">Prefer GPU (Kokoro: DirectML/CoreML · Chatterbox: CUDA/MPS) — falls back to CPU</option></select></div>
      <div>Kokoro model variant</div><div><select id="s-var">${variants.map((v) => `<option value="${v.id}" ${v.installed ? "" : "disabled"}>${esc(v.title)}${v.installed ? "" : " (not installed)"}</option>`).join("")}</select></div>
      <div>Unload models when idle</div><div><select id="s-idle"><option value="60">after 1 minute</option><option value="300">after 5 minutes</option><option value="900">after 15 minutes</option><option value="0">never (until exit)</option></select>
        <span class="hint">Nothing is loaded until MAKE; unloading frees RAM/VRAM.</span></div>
      <div>Segment cache</div><div><button id="s-cache">Clear cache</button> <span class="hint">Unchanged segments are reused; the key covers text, model, voice/reference and settings.</span></div></div></div>
    <div class="row"><button id="s-save" class="primary">Save settings</button></div>`;
  $<HTMLSelectElement>("s-dev").value = s.device;
  $<HTMLSelectElement>("s-var").value = s.kokoro_variant;
  $<HTMLSelectElement>("s-idle").value = String(s.idle_unload_secs);
  let out = s.output_dir;
  $("s-out").onclick = async () => {
    const d = await openDialog({ directory: true, title: "Output folder" });
    if (d) {
      out = d as string;
      el.querySelector("code")!.textContent = out;
    }
  };
  $("s-open-out").onclick = () => api.openFolder("output");
  $("s-cache").onclick = async () => toast(`Freed ${(await api.clearCache()).toFixed(1)} MB`, "ok");
  $("s-save").onclick = async () => {
    const n: Settings = {
      ...s,
      output_dir: out,
      export_mp3: $<HTMLInputElement>("s-mp3").checked,
      ffmpeg_path: $<HTMLInputElement>("s-ff").value.trim() || null,
      loudness_target_lufs: Number($<HTMLInputElement>("s-lufs").value),
      device: $<HTMLSelectElement>("s-dev").value,
      kokoro_variant: $<HTMLSelectElement>("s-var").value,
      idle_unload_secs: Number($<HTMLSelectElement>("s-idle").value),
    };
    try {
      await api.saveSettings(n);
      toast("Settings saved", "ok");
      onChange();
    } catch (e) {
      toast(String(e), "error");
    }
  };
}

// ---------------------------------------------------------------- Diagnostics

async function renderDiag() {
  const el = panel("diag");
  el.innerHTML = `<p class="hint">Sampling (1 s)…</p>`;
  const d = await api.diagnostics();
  const procs = (d.processes || []).map((p: any) => `pid ${p.pid}: ${p.rss_mb.toFixed(0)} MB RSS, ${p.cpu_percent.toFixed(1)}% CPU`).join("<br>");
  el.innerHTML = `
    <div class="box"><h4>Now</h4><div class="kv">
      <div>Processes</div><div>${procs}</div>
      <div>Loaded models</div><div>${d.loaded_engines.length ? d.loaded_engines.map((e: any) => esc(`${e[0]} on ${e[1]}`)).join(", ") : "none (idle)"}</div>
      <div>Busy</div><div>${esc(d.busy ?? "no")}</div>
      <div>GPU memory used (whole GPU)</div><div>${d.gpu_memory_used_mb != null ? d.gpu_memory_used_mb + " MB" : "n/a"}</div></div>
      <div class="row" style="margin-top:8px"><button id="d-unload">Unload models now</button><button id="d-refresh">Refresh</button><button id="d-copy">Copy report</button><button id="d-logs">Open logs folder</button></div></div>
    <div class="box"><h4>System</h4><div class="kv">
      <div>App</div><div>${esc(d.app_version)} · ${esc(d.platform)}</div>
      <div>OS</div><div>${esc(d.system.os)}</div><div>CPU</div><div>${esc(d.system.cpu)} (${d.system.physical_cpus} cores / ${d.system.logical_cpus} threads)</div>
      <div>RAM</div><div>${d.system.total_ram_gb.toFixed(1)} GB total, ${d.system.available_ram_gb.toFixed(1)} GB available</div>
      <div>GPU</div><div>${esc((d.system.gpus || []).join(", ") || "none detected")}</div>
      <div>ONNX Runtime</div><div>${esc(d.ort_library ?? "not installed")}</div>
      <div>Chatterbox runtime</div><div>${d.chatterbox_runtime ? esc(`Python ${d.chatterbox_runtime.python}, torch ${d.chatterbox_runtime.torch}, CUDA ${d.chatterbox_runtime.cuda}`) : "not installed"}</div>
      <div>FFmpeg</div><div>${esc(d.ffmpeg ?? "not found")} ${d.mp3_supported ? "(MP3 ok)" : ""}</div>
      <div>Models / cache</div><div>${d.models_size_mb.toFixed(0)} MB / ${d.cache_size_mb.toFixed(0)} MB · free ${d.free_space_gb?.toFixed(1)} GB</div>
      ${Object.entries(d.paths).map(([k, v]) => `<div>${esc(k)} folder</div><div><code>${esc(v)}</code></div>`).join("")}</div></div>
    <div class="box"><h4>Recent log</h4><pre class="log">${esc(d.recent_log || "(empty)")}</pre></div>`;
  $("d-unload").onclick = async () => {
    await api.unloadModels();
    setTimeout(renderDiag, 800);
  };
  $("d-refresh").onclick = renderDiag;
  $("d-logs").onclick = () => api.openFolder("logs");
  $("d-copy").onclick = () => {
    navigator.clipboard.writeText(JSON.stringify(d, null, 2));
    toast("Diagnostics copied", "ok");
  };
}

async function renderAbout() {
  const info = await api.appInfo();
  panel("about").innerHTML = `
    <div class="box"><h4>Voice Studio ${esc(info.version)}</h4>
      <p>Offline expressive text-to-speech. No account, no cloud: after models are downloaded every render happens on this computer.</p>
      <ul>
        <li><b>Kokoro 82M</b> (Apache-2.0, hexgrad) runs natively through ONNX Runtime (MIT). Pronunciation lexicon from misaki (Apache-2.0).</li>
        <li><b>Chatterbox</b> (MIT, Resemble AI) runs in an optional isolated Python worker with PyTorch (BSD-3). Its output contains Resemble AI's Perth imperceptible watermark.</li>
        <li>Fallback letter-to-sound rules adapted from the public-domain NRL report 7948 (1976).</li>
      </ul>
      <p class="hint">Voice cloning must only be used with recordings you own or have explicit permission to use. Do not use synthetic voices to deceive or impersonate. Never assume an exact voice match.</p>
      <p class="hint">Data folder: <code>${esc(info.data_root)}</code></p></div>`;
}
