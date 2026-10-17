import { invoke } from "@tauri-apps/api/core";

export type Emotion = "neutral" | "excited" | "calm" | "mystery";
export type Fidelity = "genuine" | "approximate" | "unsupported" | "none";
export type CloneMode = "voice_only" | "voice_style";

export interface Settings {
  schema_version: number;
  models_dir: string;
  output_dir: string;
  pause_short_ms: number;
  pause_long_ms: number;
  pause_paragraph_ms: number;
  segment_gap_ms: number;
  fade_ms: number;
  loudness_target_lufs: number;
  peak_ceiling_db: number;
  device: string;
  idle_unload_secs: number;
  ffmpeg_path: string | null;
  export_mp3: boolean;
  advanced_mode: boolean;
  last_engine: string;
  last_voice: string;
  last_profile: string | null;
  kokoro_variant: string;
  chatterbox_seed: number;
  consent_acknowledged: boolean;
  runtimes_dir: string | null;
}

export interface RenderOptions {
  title: string;
  engine: string;
  voice: string;
  emotion_control: boolean;
  voice_clone: boolean;
  profile_id: string | null;
  clone_mode: CloneMode;
  seed: number | null;
  speed: number;
  export_mp3: boolean | null;
  bypass_cache?: boolean;
}

export interface Caps {
  engine: string;
  cloning: boolean;
  clone_modes: CloneMode[];
  emotion: Fidelity;
  emphasis: Fidelity;
  sample_rate: number;
  notes: string[];
}

export interface EngineInfo {
  id: string;
  name: string;
  ready: boolean;
  missing: string[];
  caps: Caps;
  unsupported?: string | null;
}

export interface EngineStatus {
  engines: EngineInfo[];
  loaded: [string, string][];
  busy: string | null;
  batch_running: boolean;
  mp3: boolean;
}

export interface ProjectSummary {
  id: string;
  title: string;
  dir: string;
  final_wav: string;
  final_mp3: string | null;
  created_at: string;
  duration_secs: number;
  engine: string;
  voice: string;
  script_preview: string;
}

export interface QualityReport {
  duration_secs: number;
  source_sample_rate: number;
  peak_dbfs: number;
  rms_dbfs: number;
  clipped_ratio: number;
  silence_ratio: number;
  noise_floor_dbfs: number;
  snr_estimate_db: number;
  loudness_lufs: number | null;
  verdict: "good" | "usable" | "poor";
  issues: string[];
}

export interface VoiceProfile {
  id: string;
  name: string;
  created_at: string;
  source_file: string;
  duration_secs: number;
  quality: QualityReport;
  consent: { acknowledged: boolean; statement: string; at: string };
}

export interface ReferencePreview {
  duration_secs: number;
  source_sample_rate: number;
  suggested_start: number;
  suggested_end: number;
  peaks: [number, number][];
  quality: QualityReport;
}

export interface PackageStatus {
  id: string;
  title: string;
  kind: string;
  engine: string;
  license: string;
  note: string | null;
  installed: boolean;
  partial_bytes: number;
  download_size: number;
  disk_size: number;
  path: string;
  supported: boolean;
}

export interface Job {
  id: string;
  title: string;
  status: "pending" | "running" | "done" | "failed" | "cancelled";
  attempts: number;
  error: string | null;
  project_dir: string | null;
  created_at: string;
}

export const api = {
  appInfo: () => invoke<any>("app_info"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (s: Settings) => invoke<Settings>("save_settings", { new: s }),
  moveModelsDir: (dir: string, moveExisting: boolean) => invoke<Settings>("move_models_dir", { dir, moveExisting }),
  parseScript: (script: string, engineId: string, emotionControl: boolean, cloneMode: CloneMode | null) =>
    invoke<any>("parse_script", { script, engineId, emotionControl, cloneMode }),
  catalog: () => invoke<any>("catalog"),
  installPackage: (id: string) => invoke<void>("install_package", { id }),
  cancelDownload: (id: string) => invoke<void>("cancel_download", { id }),
  removePackage: (id: string) => invoke<void>("remove_package", { id }),
  verifyPackage: (id: string) => invoke<string[]>("verify_package", { id }),
  importPackage: (id: string, dir: string) => invoke<number>("import_package", { id, dir }),
  runtimeStatus: () => invoke<any>("runtime_status"),
  installRuntime: () => invoke<void>("install_runtime"),
  removeRuntime: () => invoke<void>("remove_runtime"),
  voices: () => invoke<{ kokoro: any[]; profiles: VoiceProfile[] }>("voices"),
  engineStatus: () => invoke<EngineStatus>("engine_status"),
  make: (script: string, options: RenderOptions, id: string) => invoke<string>("make", { script, options, id }),
  cancelRender: () => invoke<void>("cancel_render"),
  unloadModels: () => invoke<void>("unload_models"),
  queueList: () => invoke<{ jobs: Job[]; running: boolean }>("queue_list"),
  queueAdd: (text: string | null, files: string[] | null, options: RenderOptions) => invoke<number>("queue_add", { text, files, options }),
  queueStart: () => invoke<void>("queue_start"),
  queuePause: () => invoke<void>("queue_pause"),
  queueRetry: (id: string | null) => invoke<void>("queue_retry", { id }),
  queueCancel: (id: string | null) => invoke<void>("queue_cancel", { id }),
  queueRemove: (id: string) => invoke<void>("queue_remove", { id }),
  queueClearFinished: () => invoke<void>("queue_clear_finished"),
  projects: (query: string) => invoke<ProjectSummary[]>("projects", { query }),
  projectGet: (dir: string) => invoke<any>("project_get", { dir }),
  projectRename: (dir: string, title: string) => invoke<void>("project_rename", { dir, title }),
  projectDelete: (dir: string) => invoke<void>("project_delete", { dir }),
  readAudio: (path: string) => invoke<ArrayBuffer>("read_audio", { path }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  openFolder: (which: string) => invoke<void>("open_folder", { which }),
  exportAudio: (src: string, dest: string) => invoke<string>("export_audio", { src, dest }),
  referenceLoad: (path: string) => invoke<ReferencePreview>("reference_load", { path }),
  referenceClip: (start: number, end: number) => invoke<{ quality: QualityReport; wav_b64: string }>("reference_clip", { start, end }),
  profileSave: (name: string, start: number, end: number, consent: boolean) => invoke<VoiceProfile>("profile_save", { name, start, end, consent }),
  profileRename: (id: string, name: string) => invoke<void>("profile_rename", { id, name }),
  profileDelete: (id: string) => invoke<void>("profile_delete", { id }),
  diagnostics: () => invoke<any>("diagnostics"),
  clearCache: () => invoke<number>("clear_cache"),
};

export function esc(s: unknown): string {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

export function fmtBytes(n: number): string {
  if (!n) return "0 MB";
  if (n >= 1e9) return (n / 1e9).toFixed(2) + " GB";
  return (n / 1e6).toFixed(1) + " MB";
}

export function fmtTime(s: number): string {
  if (!isFinite(s) || s < 0) s = 0;
  const m = Math.floor(s / 60);
  const r = Math.floor(s % 60);
  return `${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
}

export function fidelityBadge(f: Fidelity, label: string): string {
  const txt: Record<Fidelity, string> = { genuine: "genuine", approximate: "approximated", unsupported: "not supported", none: "—" };
  return `<span class="badge ${f}" title="${esc(label)}: ${txt[f]}">${esc(label)}: ${txt[f]}</span>`;
}
