import { invoke } from "@tauri-apps/api/core";
import { store, type PackManifest } from "./store";

export async function refreshMods(): Promise<void> {
  const refreshed: any[] = await invoke("scan_local_mods");
  store.mods = refreshed;
  if (store.selectedMod && !refreshed.some((m: any) => m.id === store.selectedMod.id)) {
    store.selectedMod = null;
  }
}

export async function applyPack(pack: PackManifest): Promise<void> {
  const name = `pack:${pack.pack_id}`;
  const modIds = pack.mod_ids.map((id) => `${pack.pack_id}/${id}`);
  await invoke("apply_preset", { name, modIds });
  store.activePresetName = name;
  await refreshMods();
}
