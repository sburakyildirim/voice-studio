// Winamp-style output playlist: play / pause / seek / replay / prev / next, search, rename,
// reveal, export, delete. Audio bytes come from Rust (no file:// access needed); the
// spectrum only animates while playing, so the player costs nothing at idle.
import { save, ask } from "@tauri-apps/plugin-dialog";
import { api, esc, fmtTime, ProjectSummary } from "./api";
import { toast } from "./toast";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

export class Player {
  items: ProjectSummary[] = [];
  sel = -1;
  playingDir: string | null = null;
  audio = new Audio();
  url: string | null = null;
  ctx: AudioContext | null = null;
  analyser: AnalyserNode | null = null;
  raf = 0;
  seeking = false;
  onDetails: (dir: string) => void = () => {};

  constructor() {
    this.audio.preload = "auto";
    this.audio.volume = 0.9;
    $("p-play").onclick = () => this.toggle();
    $("p-stop").onclick = () => this.stop();
    $("p-prev").onclick = () => this.step(-1);
    $("p-next").onclick = () => this.step(1);
    $("p-replay").onclick = () => this.replay();
    const vol = $<HTMLInputElement>("vol");
    vol.oninput = () => (this.audio.volume = Number(vol.value));
    const seek = $<HTMLInputElement>("seek");
    seek.oninput = () => {
      this.seeking = true;
      if (this.audio.duration) $("lcd-time").textContent = fmtTime((Number(seek.value) / 1000) * this.audio.duration);
    };
    seek.onchange = () => {
      if (this.audio.duration) this.audio.currentTime = (Number(seek.value) / 1000) * this.audio.duration;
      this.seeking = false;
    };
    this.audio.ontimeupdate = () => this.tick();
    this.audio.onended = () => {
      this.state("STOP");
      this.stopViz();
      $("p-play").textContent = "▶";
      this.renderList();
    };
    let t: number | undefined;
    $<HTMLInputElement>("pl-search").oninput = () => {
      clearTimeout(t);
      t = window.setTimeout(() => this.refresh(), 200);
    };
    $("pl-refresh").onclick = () => this.refresh();
    $("pl-rename").onclick = () => this.rename();
    $("pl-reveal").onclick = () => this.reveal();
    $("pl-export").onclick = () => this.export();
    $("pl-delete").onclick = () => this.remove();
    $("pl-info").onclick = () => this.current() && this.onDetails(this.current()!.dir);
    const list = $("playlist");
    list.onkeydown = (e) => {
      if (e.key === "ArrowDown") { this.select(Math.min(this.items.length - 1, this.sel + 1)); e.preventDefault(); }
      else if (e.key === "ArrowUp") { this.select(Math.max(0, this.sel - 1)); e.preventDefault(); }
      else if (e.key === "Enter") { this.playIndex(this.sel); e.preventDefault(); }
      else if (e.key === " ") { this.toggle(); e.preventDefault(); }
      else if (e.key === "F2") { this.rename(); e.preventDefault(); }
      else if (e.key === "Delete") { this.remove(); e.preventDefault(); }
    };
  }

  current(): ProjectSummary | null {
    return this.items[this.sel] ?? null;
  }

  async refresh(selectDir?: string) {
    try {
      this.items = await api.projects($<HTMLInputElement>("pl-search").value);
    } catch (e) {
      toast(String(e), "error");
      this.items = [];
    }
    if (selectDir) this.sel = this.items.findIndex((i) => i.dir === selectDir);
    if (this.sel >= this.items.length) this.sel = this.items.length - 1;
    this.renderList();
  }

  renderList() {
    const list = $("playlist");
    if (!this.items.length) {
      list.innerHTML = `<div class="pl-empty">No renders yet. Write a script and press <b>MAKE</b>.</div>`;
      return;
    }
    list.innerHTML = this.items
      .map((it, i) => {
        const d = new Date(it.created_at);
        return `<div class="pl-item ${i === this.sel ? "sel" : ""} ${it.dir === this.playingDir && !this.audio.paused ? "playing" : ""}" role="option" aria-selected="${i === this.sel}" data-i="${i}">
          <span>${String(i + 1).padStart(2, "0")}.</span><span class="t">${esc(it.title)}</span><span>${fmtTime(it.duration_secs)}</span>
          <span class="meta">${esc(it.engine)} · ${esc(it.voice)} · ${d.toLocaleString()}${it.final_mp3 ? " · mp3" : ""}</span></div>`;
      })
      .join("");
    list.querySelectorAll<HTMLElement>(".pl-item").forEach((el) => {
      const i = Number(el.dataset.i);
      el.onclick = () => this.select(i);
      el.ondblclick = () => this.playIndex(i);
    });
  }

  select(i: number) {
    this.sel = i;
    this.renderList();
    $("playlist").querySelector(".sel")?.scrollIntoView({ block: "nearest" });
  }

  async playIndex(i: number) {
    const it = this.items[i];
    if (!it) return;
    this.sel = i;
    try {
      const buf = await api.readAudio(it.final_wav);
      if (this.url) URL.revokeObjectURL(this.url);
      this.url = URL.createObjectURL(new Blob([buf], { type: "audio/wav" }));
      this.audio.src = this.url;
      this.playingDir = it.dir;
      const t = $("lcd-title");
      t.textContent = `${i + 1}. ${it.title} (${fmtTime(it.duration_secs)}) *** `;
      t.parentElement!.classList.toggle("scroll", it.title.length > 26);
      await this.audio.play();
      this.startViz();
      this.state("PLAY");
      $("p-play").textContent = "⏸";
    } catch (e) {
      toast(`Cannot play: ${e}`, "error");
    }
    this.renderList();
  }

  async toggle() {
    if (!this.audio.src || this.playingDir !== this.current()?.dir) {
      if (this.sel < 0 && this.items.length) this.sel = 0;
      return this.playIndex(this.sel);
    }
    if (this.audio.paused) {
      await this.audio.play();
      this.startViz();
      this.state("PLAY");
      $("p-play").textContent = "⏸";
    } else {
      this.audio.pause();
      this.stopViz();
      this.state("PAUSE");
      $("p-play").textContent = "▶";
    }
    this.renderList();
  }

  stop() {
    this.audio.pause();
    this.audio.currentTime = 0;
    this.stopViz();
    this.state("STOP");
    $("p-play").textContent = "▶";
    this.tick();
    this.renderList();
  }

  replay() {
    if (!this.audio.src) return this.playIndex(this.sel);
    this.audio.currentTime = 0;
    if (this.audio.paused) this.toggle();
  }

  step(d: number) {
    if (!this.items.length) return;
    const i = (Math.max(0, this.sel) + d + this.items.length) % this.items.length;
    this.playIndex(i);
  }

  tick() {
    const dur = this.audio.duration || 0;
    if (!this.seeking) {
      $("lcd-time").textContent = fmtTime(this.audio.currentTime);
      $<HTMLInputElement>("seek").value = String(dur ? Math.round((this.audio.currentTime / dur) * 1000) : 0);
    }
  }

  state(s: string) {
    $("lcd-state").textContent = s;
  }

  startViz() {
    try {
      if (!this.ctx) {
        this.ctx = new AudioContext();
        const src = this.ctx.createMediaElementSource(this.audio);
        this.analyser = this.ctx.createAnalyser();
        this.analyser.fftSize = 128;
        src.connect(this.analyser);
        this.analyser.connect(this.ctx.destination);
      }
      this.ctx.resume();
    } catch {
      return;
    }
    const cv = $<HTMLCanvasElement>("viz");
    const g = cv.getContext("2d")!;
    const data = new Uint8Array(this.analyser!.frequencyBinCount);
    const draw = () => {
      this.analyser!.getByteFrequencyData(data);
      g.clearRect(0, 0, cv.width, cv.height);
      const bars = 20;
      const w = cv.width / bars;
      for (let b = 0; b < bars; b++) {
        const v = data[Math.floor((b / bars) * data.length * 0.7)] / 255;
        const h = Math.max(1, v * cv.height);
        g.fillStyle = v > 0.8 ? "#ffc45c" : "#7dff9a";
        g.fillRect(b * w + 1, cv.height - h, w - 2, h);
      }
      this.raf = requestAnimationFrame(draw);
    };
    cancelAnimationFrame(this.raf);
    draw();
  }

  stopViz() {
    cancelAnimationFrame(this.raf);
    const cv = $<HTMLCanvasElement>("viz");
    cv.getContext("2d")!.clearRect(0, 0, cv.width, cv.height);
    this.ctx?.suspend();
  }

  async rename() {
    const it = this.current();
    if (!it) return;
    const el = $("playlist").querySelector(".sel .t") as HTMLElement | null;
    if (!el) return;
    const input = document.createElement("input");
    input.type = "text";
    input.value = it.title;
    input.style.width = "100%";
    el.replaceWith(input);
    input.focus();
    input.select();
    let done = false;
    const finish = async (commit: boolean) => {
      if (done) return;
      done = true;
      if (commit && input.value.trim() && input.value !== it.title) {
        try {
          await api.projectRename(it.dir, input.value.trim());
        } catch (e) {
          toast(String(e), "error");
        }
      }
      await this.refresh(it.dir);
      $("playlist").focus();
    };
    input.onkeydown = (e) => {
      e.stopPropagation();
      if (e.key === "Enter") finish(true);
      if (e.key === "Escape") finish(false);
    };
    input.onblur = () => finish(true);
  }

  async reveal() {
    const it = this.current();
    if (it) api.reveal(it.final_wav).catch((e) => toast(String(e), "error"));
  }

  async export() {
    const it = this.current();
    if (!it) return;
    const mp3 = (await api.engineStatus()).mp3;
    const dest = await save({
      defaultPath: `${it.title.replace(/[\\/:*?"<>|]/g, "_")}.wav`,
      filters: mp3 ? [{ name: "WAV audio", extensions: ["wav"] }, { name: "MP3 audio (FFmpeg)", extensions: ["mp3"] }] : [{ name: "WAV audio", extensions: ["wav"] }],
    });
    if (!dest) return;
    try {
      const out = await api.exportAudio(it.final_wav, dest);
      toast(`Exported to ${out}`, "ok");
    } catch (e) {
      toast(String(e), "error");
    }
  }

  async remove() {
    const it = this.current();
    if (!it) return;
    const ok = await ask(`Delete "${it.title}" and its project folder (script, segments, renders)? This cannot be undone.`, { title: "Delete render", kind: "warning" });
    if (!ok) return;
    if (this.playingDir === it.dir) {
      this.stop();
      this.audio.removeAttribute("src");
      this.playingDir = null;
    }
    try {
      await api.projectDelete(it.dir);
    } catch (e) {
      toast(String(e), "error");
    }
    this.refresh();
  }
}
