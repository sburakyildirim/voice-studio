import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api, esc, EngineStatus, fidelityBadge, fmtTime, RenderOptions, Settings, VoiceProfile, CloneMode } from "./api";
import { highlight, wrapSelection } from "./highlight";
import { Player } from "./player";
import { initOptions, openOptions } from "./options";
import { toast } from "./toast";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const ta = $<HTMLTextAreaElement>("script");
const hl = $<HTMLPreElement>("hl");
const DRAFT_KEY = "vs.draft";

let settings: Settings;
let status: EngineStatus;
let profiles: VoiceProfile[] = [];
let kokoroVoices: any[] = [];
let cloneMode: CloneMode = "voice_only";
let currentJob: string | null = null;
let advanced = false;
const player = new Player();

const EXAMPLE = `"Stop scrolling! This changes everything." -- Most people drink water wrong. [Here is the simple fix.] Drink one glass right after you wake up.

{But there is one thing nobody tells you...} --- *Temperature matters.* Room temperature water is absorbed faster than ice cold water. "Try it for one week and tell me what happens!"`;

// ------------------------------------------------------------------ editor

function syncHighlight() {
  hl.innerHTML = highlight(ta.value);
  hl.scrollTop = ta.scrollTop;
  const words = ta.value.replace(/\\./g, "").replace(/[\"\[\]{}*]|-{2,}/g, " ").split(/\s+/).filter(Boolean).length;
  const est = (words / 2.6) * (Number($<HTMLInputElement>("a-sp").value) > 0 ? 1 / Number($<HTMLInputElement>("a-sp").value) : 1);
  $("stats").textContent = `${words} words · ≈ ${fmtTime(est)} spoken`;
  try {
    localStorage.setItem(DRAFT_KEY, ta.value);
  } catch {}
  scheduleParse();
}

let parseTimer: number | undefined;
function scheduleParse() {
  clearTimeout(parseTimer);
  parseTimer = window.setTimeout(async () => {
    const r = await api.parseScript(ta.value, engineId(), emotionOn(), cloneOn() ? cloneMode : null).catch(() => null);
    const w = $("warnings");
    if (r && r.warnings.length) {
      w.hidden = false;
      w.textContent = `⚠ ${r.warnings.length} markup warning${r.warnings.length > 1 ? "s" : ""}`;
      w.title = r.warnings.map((x: any) => x.message).join("\n");
      w.onclick = () => toast(r.warnings.map((x: any) => `• ${x.message}`).join("\n"), "info", 10000);
    } else w.hidden = true;
    if (advanced && r) renderInspector(r);
  }, 250);
}

ta.addEventListener("input", syncHighlight);
ta.addEventListener("scroll", () => (hl.scrollTop = ta.scrollTop));
new ResizeObserver(() => (hl.scrollTop = ta.scrollTop)).observe(ta);

document.querySelectorAll<HTMLButtonElement>(".legend .lg").forEach((b) => {
  b.onmousedown = (e) => e.preventDefault(); // keep the textarea selection
  b.onclick = () => wrapSelection(ta, b.dataset.tag!);
});

$("btn-example").onclick = () => {
  ta.value = ta.value.trim() ? `${ta.value}\n\n${EXAMPLE}` : EXAMPLE;
  syncHighlight();
};
$("btn-open-script").onclick = async () => {
  const f = await openDialog({ filters: [{ name: "Text", extensions: ["txt", "md"] }] });
  if (!f) return;
  try {
    const { readTextFileViaRust } = await import("./readfile");
    ta.value = await readTextFileViaRust(f as string);
    syncHighlight();
  } catch (e) {
    toast(String(e), "error");
  }
};

// ------------------------------------------------------------------ controls

const engineId = () => $<HTMLSelectElement>("engine").value || "kokoro";
const emotionOn = () => $<HTMLInputElement>("t-emotion").checked;
const cloneOn = () => $<HTMLInputElement>("t-clone").checked;
const batchOn = () => $<HTMLInputElement>("t-batch").checked;

function engineInfo(id = engineId()) {
  return status?.engines.find((e) => e.id === id);
}

function renderEngineSelect() {
  const sel = $<HTMLSelectElement>("engine");
  const cur = sel.value || settings.last_engine;
  sel.innerHTML = status.engines
    .map((e) => `<option value="${e.id}">${esc(e.name)}${e.unsupported ? " — not supported here" : e.ready ? "" : " — not installed"}</option>`)
    .join("");
  sel.value = status.engines.some((e) => e.id === cur) ? cur : "kokoro";
}

function renderVoiceSelect() {
  const sel = $<HTMLSelectElement>("voice");
  const label = $("voice-label");
  const clone = cloneOn();
  if (clone) {
    label.textContent = "Voice profile (reference)";
    sel.innerHTML = profiles.length
      ? profiles.map((p) => `<option value="${p.id}">${esc(p.name)} · ${p.duration_secs.toFixed(0)} s · ${p.quality.verdict}</option>`).join("")
      : `<option value="">No voice profiles — add one in Options › Voices</option>`;
    if (settings.last_profile && profiles.some((p) => p.id === settings.last_profile)) sel.value = settings.last_profile;
  } else if (engineId() === "kokoro") {
    label.textContent = "Voice";
    sel.innerHTML = kokoroVoices.length
      ? kokoroVoices.map((v) => `<option value="${v.id}">${esc(v.name)} — ${esc(v.accent)} ${esc(v.gender)} (${v.id})</option>`).join("")
      : `<option value="">Install Kokoro voices in Options › Models</option>`;
    if (kokoroVoices.some((v) => v.id === settings.last_voice)) sel.value = settings.last_voice;
  } else {
    label.textContent = "Voice";
    sel.innerHTML = `<option value="builtin">Chatterbox built-in voice</option>`;
  }
}

function renderCaps() {
  const e = engineInfo();
  const caps = $("caps");
  const cloneT = $<HTMLInputElement>("t-clone");
  const canClone = !!e?.caps.cloning;
  cloneT.disabled = !canClone;
  cloneT.parentElement!.classList.toggle("disabled", !canClone);
  cloneT.parentElement!.title = canClone ? "Use a voice profile as the speaker" : `${e?.name ?? "This engine"} does not support reference voice cloning`;
  if (!canClone && cloneT.checked) cloneT.checked = false;
  $("clone-mode").hidden = !cloneT.checked;
  if (!e) {
    caps.innerHTML = "";
    return;
  }
  const emo: any = emotionOn() ? (cloneOn() && cloneMode === "voice_style" ? "unsupported" : e.caps.emotion) : "none";
  if (e.unsupported) {
    caps.innerHTML = `<span class="badge unsupported" title="${esc(e.unsupported)}">not supported on this computer</span> <span class="hint">${esc(e.unsupported)}</span>`;
    $<HTMLButtonElement>("btn-make").disabled = true;
    return;
  }
  const missing = e.ready ? "" : `<span class="badge unsupported">missing: ${esc(e.missing.join(", "))}</span> <button class="link" id="caps-install">Install…</button>`;
  caps.innerHTML = `${missing}
    ${fidelityBadge(emo, "Emotion")}
    ${fidelityBadge(e.caps.emphasis, "Emphasis")}
    ${fidelityBadge(e.caps.cloning ? "genuine" : "unsupported", "Voice cloning")}
    ${cloneOn() ? fidelityBadge(cloneMode === "voice_style" ? "approximate" : "genuine", cloneMode === "voice_style" ? "Reference style" : "Speaker identity") : ""}
    <span class="badge" title="${esc(e.caps.notes.join("\n"))}">ⓘ details</span>`;
  caps.querySelector<HTMLElement>('[title]:last-child')!.onclick = () => toast(e.caps.notes.map((n) => `• ${n}`).join("\n"), "info", 12000);
  document.getElementById("caps-install")?.addEventListener("click", () => openOptions("models"));
  $<HTMLButtonElement>("btn-make").disabled = !!currentJob;
}

$("engine").onchange = () => {
  settings.last_engine = engineId();
  persistSettings();
  renderVoiceSelect();
  renderCaps();
  scheduleParse();
};
$("voice").onchange = () => {
  const v = $<HTMLSelectElement>("voice").value;
  if (cloneOn()) settings.last_profile = v;
  else if (engineId() === "kokoro") settings.last_voice = v;
  persistSettings();
};
$("t-clone").onchange = () => {
  renderVoiceSelect();
  renderCaps();
  scheduleParse();
};
$("t-emotion").onchange = () => {
  renderCaps();
  scheduleParse();
};
$("t-batch").onchange = () => {
  $("make-area").hidden = batchOn();
  $("batch-area").hidden = !batchOn();
  if (batchOn()) refreshQueue();
};
document.querySelectorAll<HTMLButtonElement>("#clone-mode button").forEach((b) => {
  b.onclick = () => {
    cloneMode = b.dataset.mode as CloneMode;
    document.querySelectorAll("#clone-mode button").forEach((x) => {
      x.classList.toggle("on", x === b);
      x.setAttribute("aria-checked", String(x === b));
    });
    renderCaps();
    scheduleParse();
  };
});

function setMode(adv: boolean) {
  advanced = adv;
  $("mode-simple").classList.toggle("on", !adv);
  $("mode-advanced").classList.toggle("on", adv);
  $("mode-simple").setAttribute("aria-checked", String(!adv));
  $("mode-advanced").setAttribute("aria-checked", String(adv));
  $("advanced").hidden = !adv;
  if (settings && settings.advanced_mode !== adv) {
    settings.advanced_mode = adv;
    persistSettings();
  }
  if (adv) scheduleParse();
}
$("mode-simple").onclick = () => setMode(false);
$("mode-advanced").onclick = () => setMode(true);

// Advanced sliders write straight into settings (debounced save).
const sliders: [string, keyof Settings, string][] = [
  ["a-ps", "pause_short_ms", "v-ps"],
  ["a-pl", "pause_long_ms", "v-pl"],
  ["a-pp", "pause_paragraph_ms", "v-pp"],
  ["a-pg", "segment_gap_ms", "v-pg"],
];
function bindAdvanced() {
  for (const [id, key, lbl] of sliders) {
    const el = $<HTMLInputElement>(id);
    el.value = String(settings[key]);
    $(lbl).textContent = `${el.value} ms`;
    el.oninput = () => {
      ($(lbl).textContent = `${el.value} ms`), ((settings as any)[key] = Number(el.value));
      persistSettings();
      scheduleParse();
    };
  }
  const sp = $<HTMLInputElement>("a-sp");
  $("v-sp").textContent = `${Number(sp.value).toFixed(2)}×`;
  sp.oninput = () => {
    $("v-sp").textContent = `${Number(sp.value).toFixed(2)}×`;
    syncHighlight();
  };
  const lufs = $<HTMLInputElement>("a-lufs");
  lufs.value = String(settings.loudness_target_lufs);
  lufs.onchange = () => {
    settings.loudness_target_lufs = Number(lufs.value);
    persistSettings();
  };
  const seed = $<HTMLInputElement>("a-seed");
  seed.value = String(settings.chatterbox_seed);
  seed.onchange = () => {
    settings.chatterbox_seed = Math.max(0, Math.floor(Number(seed.value)));
    persistSettings();
  };
}

let saveTimer: number | undefined;
function persistSettings() {
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(async () => {
    try {
      settings = await api.saveSettings(settings);
    } catch (e) {
      toast(String(e), "error");
    }
  }, 400);
}

function renderInspector(r: any) {
  const el = $("inspector");
  if (!r.segments.length) {
    el.innerHTML = `<p class="hint">No speakable text.</p>`;
    return;
  }
  el.innerHTML = `<p class="hint">${r.word_count} words → ${r.segments.length} segments for ${esc(engineId())}.</p>
  <table><thead><tr><th>#</th><th>Emotion</th><th>Text</th><th>Pause after</th><th>Controls</th></tr></thead><tbody>
  ${r.segments.map((s: any) => `<tr><td>${s.index}</td><td class="emo-${s.emotion}">${s.emotion}${s.emphasis ? " +emph" : ""}</td><td>${esc(s.text)}</td>
    <td>${Math.round(s.pause_after_ms)} ms${s.pause_kind ? ` (${s.pause_kind})` : ""}</td>
    <td>${fidelityBadge(s.emotion_fidelity, "emotion")}<br><span class="hint">${engineId() === "chatterbox" ? `exaggeration ${s.params.exaggeration}, cfg ${s.params.cfg_weight}` : `rate ${s.params.speed.toFixed(2)}×, gain ${s.params.gain_db} dB${s.params.terminal_punct ? `, end “${s.params.terminal_punct}”` : ""}`}</span></td></tr>`).join("")}
  </tbody></table>`;
}
$("btn-analyze").onclick = async () => renderInspector(await api.parseScript(ta.value, engineId(), emotionOn(), cloneOn() ? cloneMode : null));

// ------------------------------------------------------------------ MAKE

function options(): RenderOptions {
  const voice = $<HTMLSelectElement>("voice").value;
  return {
    title: $<HTMLInputElement>("a-title").value.trim(),
    engine: engineId(),
    voice: cloneOn() || engineId() !== "kokoro" ? settings.last_voice : voice,
    emotion_control: emotionOn(),
    voice_clone: cloneOn(),
    profile_id: cloneOn() ? voice || null : null,
    clone_mode: cloneMode,
    seed: advanced ? settings.chatterbox_seed : null,
    speed: Number($<HTMLInputElement>("a-sp").value) || 1,
    export_mp3: null,
  };
}

function preflight(): boolean {
  const e = engineInfo();
  if (!e) return false;
  if (!e.ready) {
    toast(`${e.name} needs downloads: ${e.missing.join(", ")}. Opening Options › Models.`, "error");
    openOptions("models");
    return false;
  }
  if (cloneOn() && !$<HTMLSelectElement>("voice").value) {
    toast("Voice Clone is on but there is no voice profile. Add one in Options › Voices.", "error");
    openOptions("voices");
    return false;
  }
  if (!ta.value.trim()) {
    toast("Write or paste a script first.", "error");
    ta.focus();
    return false;
  }
  return true;
}

async function make() {
  if (currentJob || !preflight()) return;
  try {
    currentJob = await api.make(ta.value, options());
    showProgress(true, "Starting…", 0);
  } catch (e) {
    toast(String(e), "error");
  }
}

function showProgress(on: boolean, text = "", frac = 0) {
  $("progress-wrap").hidden = !on;
  $<HTMLButtonElement>("btn-make").disabled = on;
  $("btn-make").textContent = on ? "MAKING…" : "MAKE";
  $("progress-text").textContent = text;
  $("progress-bar").style.width = `${Math.round(frac * 100)}%`;
}

$("btn-make").onclick = make;
$("btn-cancel").onclick = () => api.cancelRender();

listen<any>("render-progress", (e) => {
  const ev = e.payload.event;
  if (e.payload.batch) {
    $("q-summary").textContent = `${ev.message}`;
    return;
  }
  showProgress(true, ev.message, ev.fraction);
});

listen<any>("job-event", async (e) => {
  const j = e.payload;
  if (j.kind === "started") return;
  if (j.id === currentJob) {
    currentJob = null;
    showProgress(false);
    if (j.kind === "done") {
      toast("Render complete", "ok", 2500);
      await player.refresh(j.project_dir);
      const i = player.items.findIndex((x) => x.dir === j.project_dir);
      if (i >= 0) player.playIndex(i);
    } else if (j.kind === "cancelled") toast("Cancelled");
    else {
      toast(j.error, "error");
      if (j.missing_packages?.length) openOptions("models");
    }
    refreshStatus();
  } else {
    if (j.kind === "done") player.refresh();
    refreshQueue();
  }
});

listen<any>("engine-state", (e) => renderChip(e.payload));

function renderChip(loaded: [string, string][]) {
  const chip = $("engine-chip");
  chip.textContent = loaded.length ? `Loaded: ${loaded.map(([n, d]) => `${n} (${d})`).join(", ")}` : "Idle · no models loaded";
  chip.classList.toggle("live", loaded.length > 0);
}

// ------------------------------------------------------------------ batch

async function refreshQueue() {
  const q = await api.queueList();
  const counts: Record<string, number> = {};
  q.jobs.forEach((j) => (counts[j.status] = (counts[j.status] || 0) + 1));
  $("q-summary").textContent = `${q.jobs.length} jobs · ${Object.entries(counts).map(([k, v]) => `${v} ${k}`).join(" · ")}${q.running ? " · running" : ""}`;
  $<HTMLButtonElement>("q-start").disabled = q.running || !q.jobs.some((j) => j.status === "pending");
  $("queue").innerHTML = q.jobs.length
    ? q.jobs.map((j) => `<div class="qrow" role="listitem"><span class="qst ${j.status}">${j.status}${j.attempts > 1 ? ` ×${j.attempts}` : ""}</span><span>${esc(j.title)}</span>
        <span>${j.status === "failed" || j.status === "cancelled" ? `<button class="small" data-q="retry" data-id="${j.id}">Retry</button>` : ""}
        ${j.status === "pending" ? `<button class="small" data-q="cancel" data-id="${j.id}">Cancel</button>` : ""}
        ${j.status === "running" ? `<button class="small danger" data-q="cancel" data-id="${j.id}">Stop</button>` : `<button class="small" data-q="remove" data-id="${j.id}" aria-label="Remove">✕</button>`}</span>
        ${j.error ? `<span class="qerr">${esc(j.error)}</span>` : ""}</div>`).join("")
    : `<div class="pl-empty">Queue is empty.</div>`;
  $("queue").querySelectorAll<HTMLButtonElement>("button[data-q]").forEach((b) => {
    b.onclick = async () => {
      const id = b.dataset.id!;
      try {
        if (b.dataset.q === "retry") await api.queueRetry(id);
        if (b.dataset.q === "cancel") await api.queueCancel(id);
        if (b.dataset.q === "remove") await api.queueRemove(id);
      } catch (e) {
        toast(String(e), "error");
      }
      refreshQueue();
    };
  });
}
listen("queue-changed", () => refreshQueue());

$("q-add-current").onclick = async () => {
  if (!preflight()) return;
  try {
    const n = await api.queueAdd(ta.value, null, options());
    toast(`Added ${n} script${n === 1 ? "" : "s"} to the queue`, "ok");
  } catch (e) {
    toast(String(e), "error");
  }
  refreshQueue();
};
$("q-add-files").onclick = async () => {
  const e = engineInfo();
  if (!e?.ready) return preflight();
  const files = await openDialog({ multiple: true, filters: [{ name: "Scripts", extensions: ["txt", "md"] }] });
  if (!files) return;
  try {
    const n = await api.queueAdd(null, files as string[], options());
    toast(`Added ${n} script${n === 1 ? "" : "s"}`, "ok");
  } catch (err) {
    toast(String(err), "error");
  }
  refreshQueue();
};
$("q-start").onclick = () => api.queueStart().catch((e) => toast(String(e), "error")).then(refreshQueue);
$("q-pause").onclick = () => api.queuePause().then(refreshQueue);
$("q-cancel").onclick = async () => {
  const running = (await api.queueList()).jobs.find((j) => j.status === "running");
  if (running) await api.queueCancel(running.id);
  refreshQueue();
};
$("q-retry").onclick = () => api.queueRetry(null).then(refreshQueue);
$("q-clear").onclick = () => api.queueClearFinished().then(refreshQueue);

// ------------------------------------------------------------------ details dialog

player.onDetails = async (dir) => {
  try {
    const m = await api.projectGet(dir);
    $("details-title").textContent = m.title;
    const s = m.stats;
    $("details-body").innerHTML = `<div class="kv">
      <div>Engine / model</div><div>${esc(m.engine)} · <code>${esc(m.model)}</code></div>
      <div>Voice</div><div>${esc(m.profile_name ?? m.voice)}${m.clone_mode ? ` · ${esc(m.clone_mode)}` : ""} · emotion control ${m.emotion_control ? "on" : "off"}</div>
      <div>Duration</div><div>${s.audio_secs.toFixed(2)} s · rendered in ${s.render_secs.toFixed(2)} s (RTF ${s.rtf.toFixed(3)}) on ${esc(s.device)}</div>
      <div>Segments</div><div>${s.segments_total} (${s.segments_cached} from cache)</div>
      <div>Loudness</div><div>${s.loudness_before_lufs?.toFixed(1) ?? "—"} → ${s.loudness_after_lufs?.toFixed(1) ?? "—"} LUFS</div>
      <div>Peak memory</div><div>${s.peak_rss_mb ? Math.round(s.peak_rss_mb) + " MB RAM" : "—"}${s.peak_vram_mb ? ` · ${Math.round(s.peak_vram_mb)} MB GPU` : ""}</div>
      <div>Folder</div><div><code>${esc(dir)}</code></div></div>
      <h3 style="margin-top:14px">Segments</h3>
      <table><thead><tr><th>#</th><th>At</th><th>Emotion</th><th>Text</th><th>Length</th><th>Control</th></tr></thead><tbody>
      ${m.segments.map((g: any) => `<tr><td>${g.index}</td><td>${fmtTime(g.offset_secs)}</td><td class="emo-${g.emotion}">${g.emotion}</td><td>${esc(g.text)}</td><td>${g.duration_secs.toFixed(2)} s${g.cached ? " (cache)" : ""}</td><td>${fidelityBadge(g.emotion_fidelity, "emotion")}</td></tr>`).join("")}
      </tbody></table>
      ${m.warnings.length ? `<h3>Markup warnings</h3><ul class="issues">${m.warnings.map((w: any) => `<li>${esc(w.message)}</li>`).join("")}</ul>` : ""}`;
    $<HTMLDialogElement>("details").showModal();
  } catch (e) {
    toast(String(e), "error");
  }
};
$("details-close").onclick = () => $<HTMLDialogElement>("details").close();

// ------------------------------------------------------------------ keyboard

document.addEventListener("keydown", (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key === "Enter") {
    e.preventDefault();
    batchOn() ? $("q-add-current").click() : make();
  } else if (e.key === "Escape" && currentJob) {
    api.cancelRender();
  } else if (mod && e.key === ",") {
    e.preventDefault();
    openOptions();
  } else if (mod && document.activeElement === ta && ["1", "2", "3", "4", "5", "6"].includes(e.key)) {
    e.preventDefault();
    wrapSelection(ta, ["excited", "calm", "mystery", "emphasis", "short", "long"][Number(e.key) - 1]);
  }
});

$("btn-options").onclick = () => openOptions();

// ------------------------------------------------------------------ boot

async function refreshStatus() {
  [status, settings] = await Promise.all([api.engineStatus(), api.getSettings()]);
  const v = await api.voices();
  profiles = v.profiles;
  kokoroVoices = v.kokoro;
  renderEngineSelect();
  renderVoiceSelect();
  renderCaps();
  renderChip(status.loaded);
}

async function boot() {
  initOptions(() => refreshStatus());
  await refreshStatus();
  setMode(settings.advanced_mode);
  bindAdvanced();
  try {
    ta.value = localStorage.getItem(DRAFT_KEY) ?? "";
  } catch {}
  syncHighlight();
  await player.refresh();
  if (!status.engines.some((e) => e.ready)) {
    toast("Welcome! Download a model first: Options › Models (Kokoro is ~370 MB, works on any CPU).", "info", 9000);
    openOptions("models");
  }
}

boot().catch((e) => toast(`Startup error: ${e}`, "error"));
