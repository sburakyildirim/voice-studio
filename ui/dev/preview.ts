// Development-only UI preview: loads the real index.html body and main.ts with Tauri IPC
// mocked by canned data, so the layout can be reviewed in any browser.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import models from "../../catalog/models.json";
import packagesFile from "../../catalog/packages.json";

const html = await (await fetch("/index.html")).text();
document.body.innerHTML = new DOMParser().parseFromString(html, "text/html").body.innerHTML.replace(/<script[\s\S]*?<\/script>/g, "");

const settings = {
  schema_version: 1, models_dir: "C:\\Users\\me\\AppData\\Local\\VoiceStudio\\models", output_dir: "C:\\Users\\me\\Documents\\Voice Studio",
  pause_short_ms: 350, pause_long_ms: 900, pause_paragraph_ms: 650, segment_gap_ms: 140, fade_ms: 6, loudness_target_lufs: -16, peak_ceiling_db: -1,
  device: "auto", idle_unload_secs: 300, ffmpeg_path: null, export_mp3: false, advanced_mode: false, last_engine: "kokoro", last_voice: "af_heart",
  last_profile: null, kokoro_variant: "kokoro-82m-fp32", chatterbox_seed: 1234, consent_acknowledged: false,
};
const caps = (id: string) => id === "kokoro"
  ? { engine: "kokoro", cloning: false, clone_modes: [], emotion: "approximate", emphasis: "approximate", sample_rate: 24000, notes: ["Kokoro has no emotion conditioning."] }
  : { engine: "chatterbox", cloning: true, clone_modes: ["voice_only", "voice_style"], emotion: "genuine", emphasis: "unsupported", sample_rate: 24000, notes: ["Zero-shot cloning."] };
const projects = [
  { id: "a", title: "Chatterbox clone test", dir: "p/a", final_wav: "p/a/final.wav", final_mp3: null, created_at: new Date().toISOString(), duration_secs: 19.5, engine: "chatterbox", voice: "Synthetic Emma (Kokoro)", script_preview: "" },
  { id: "b", title: "Octopus facts — 60 second narration", dir: "p/b", final_wav: "p/b/final.wav", final_mp3: "p/b/final.mp3", created_at: new Date(Date.now() - 3.6e6).toISOString(), duration_secs: 50.6, engine: "kokoro", voice: "af_heart", script_preview: "" },
];

mockWindows("main");
mockIPC((cmd, args: any) => {
  switch (cmd) {
    case "get_settings": case "save_settings": return args?.new ?? settings;
    case "engine_status": return { engines: [
      { id: "kokoro", name: "Kokoro 82M (fast, built-in voices)", ready: true, missing: [], caps: caps("kokoro") },
      { id: "chatterbox", name: "Chatterbox (voice cloning, emotion intensity)", ready: true, missing: [], caps: caps("chatterbox") }],
      loaded: [], busy: null, batch_running: false, mp3: true };
    case "voices": return { kokoro: [{ id: "af_heart", name: "Heart", accent: "American", gender: "female" }, { id: "bf_emma", name: "Emma", accent: "British", gender: "female" }],
      profiles: [{ id: "emma", name: "Synthetic Emma (Kokoro)", created_at: new Date().toISOString(), source_file: "synthetic.wav", duration_secs: 41.1,
        quality: { verdict: "good", snr_estimate_db: 48, issues: [] }, consent: { acknowledged: true, statement: "", at: "" } }] };
    case "projects": return projects;
    case "parse_script": return { spans: [], warnings: [], segments: [], word_count: 0 };
    case "queue_list": return { jobs: [
      { id: "1", title: "Episode 1 — Octopus hearts", status: "done", attempts: 1, error: null, project_dir: "x", created_at: "" },
      { id: "2", title: "Episode 2 — Blue blood", status: "running", attempts: 1, error: null, project_dir: null, created_at: "" },
      { id: "3", title: "Episode 3 — Smart arms", status: "failed", attempts: 2, error: "Chatterbox worker exited (exit code 1).", project_dir: null, created_at: "" },
      { id: "4", title: "Episode 4", status: "pending", attempts: 0, error: null, project_dir: null, created_at: "" }], running: true };
    case "catalog": return { packages: (packagesFile as any).packages.filter((p: any) => !p.platforms?.length || p.platforms.includes("windows-x86_64")).map((p: any, i: number) => ({
        id: p.id, title: p.title, kind: p.kind, engine: p.engine, license: p.license, note: p.note ?? null, installed: i % 3 !== 1, partial_bytes: i === 1 ? 40e6 : 0,
        download_size: p.files.reduce((a: number, f: any) => a + f.size, 0), disk_size: p.files.reduce((a: number, f: any) => a + f.size, 0), path: "", supported: true })),
      models, free_space: 13e9, models_dir: settings.models_dir, benchmarks: [{ engine: "kokoro", device: "cpu:8t", warm_rtf: 0.31, peak_rss_mb: 870 }], ort_candidates: [] };
    case "runtime_status": return { installed: { python: "3.11.15", torch: "2.6.0+cu124", cuda: true, mps: false, cuda_device: "NVIDIA GeForce RTX 4050 Laptop GPU", chatterbox: "0.1.7" }, dir: "C:\…\runtimes\chatterbox", disk_size: 5.1e9, uv: "uv.exe", nvidia: true, estimated_download_gb: 3.5, installing: false };
    case "app_info": return { version: "0.1.0", consent_text: "I confirm…", data_root: "C:\\…" };
    default: return null;
  }
});

await import("../src/main");
