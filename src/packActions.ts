import { invoke } from "@tauri-apps/api/core";
import { store, type PackManifest } from "./store";

export async function loadPacks(): Promise<void> {
  store.packs = await invoke("load_packs") as PackManifest[];
  const active = store.activePresetName;
  if (active.startsWith("pack:") && !store.packs.some((p) => `pack:${p.pack_id}` === active)) {
    store.activePresetName = "";
  }
}

// Rescans mods and packs, then points the selection at the fresh copy of the selected mod or clears it when gone.
export async function refreshMods(): Promise<void> {
  const refreshed: any[] = await invoke("scan_local_mods");
  store.mods = refreshed;
  await loadPacks();
  const sel = store.selectedMod;
  if (sel && !sel.is_online) {
    store.selectedMod = refreshed.find((m: any) => m.id === sel.id) ?? null;
  }
}

export async function applyPack(stale: PackManifest): Promise<void> {
  await loadPacks();
  const pack = store.packs.find((p) => p.pack_id === stale.pack_id);
  if (!pack) throw new Error(`Modpack ${stale.name} is no longer installed`);
  const name = `pack:${pack.pack_id}`;
  const modIds = pack.mod_ids.map((id) => `${pack.pack_id}/${id}`);
  await invoke("apply_preset", { name, modIds });
  store.activePresetName = name;
  await refreshMods();
}
