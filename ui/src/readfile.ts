import { invoke } from "@tauri-apps/api/core";

export function readTextFileViaRust(path: string): Promise<string> {
  return invoke<string>("read_script_file", { path });
}
