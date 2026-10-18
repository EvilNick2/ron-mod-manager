import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { store, type PackManifest, type PackProgress } from "./store";

type PublishOutcome =
  | { kind: "Published"; folder_id: string; version: string; uploaded: number; deleted: number }
  | { kind: "NothingToPublish"; folder_id: string };

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

export async function loadGoogleStatus(): Promise<void> {
  const status: any = await invoke("google_status");
  store.google = {
    signInAvailable: status.sign_in_available,
    linksAvailable: status.links_available,
    email: status.email ?? null,
  };
}

export function isLinked(packId: string): boolean {
  return !!store.packs.find((p) => p.pack_id === packId)?.source;
}

export function isPublished(packId: string): boolean {
  return !!store.packs.find((p) => p.pack_id === packId)?.published;
}

export function folderLink(folderId: string): string {
  return `https://drive.google.com/drive/folders/${folderId}?usp=sharing`;
}

// The webview clipboard API is unreliable on WebKitGTK, so this goes through the plugin
export async function copyText(text: string): Promise<void> {
  try {
    await writeText(text);
  } catch (e) {
    console.error("Clipboard write failed:", e);
    prompt("Copy this link:", text);
  }
}

export function beginOp(packId: string): void {
  store.inFlight[packId] = true;
}

export function endOp(packId: string): void {
  delete store.inFlight[packId];
  delete store.packProgress[packId];
}

// Rounds ties to even like Rust's {:.N}. A tie is exact only when x * 2^(digits+1) is an odd integer.
function toFixedEven(x: number, digits: number): string {
  const t = x * 2 ** (digits + 1);
  if (!Number.isInteger(t) || t % 2 === 0) return x.toFixed(digits);
  const floor = Math.floor(x * 10 ** digits);
  return ((floor % 2 === 0 ? floor : floor + 1) / 10 ** digits).toFixed(digits);
}

// Mirrors format_size in publish.rs
export function formatSize(bytes: number): string {
  const mib = 1024 * 1024;
  const gib = 1024 * mib;
  if (bytes >= gib) return `${toFixedEven(bytes / gib, 1)} GB`;
  if (bytes >= mib) return `${toFixedEven(bytes / mib, 0)} MB`;
  return `${Math.ceil(bytes / 1024)} KB`;
}

export function progressLine(p: PackProgress): string {
  const count = `${p.done} of ${p.total}`;
  const file = p.file ? `: ${p.file} (${formatSize(p.bytes_total)})` : "";
  switch (p.stage) {
    case "hashing":
      return `Checking mods ${count}`;
    case "checking_drive":
      return "Checking Drive";
    case "deleting":
      return `Deleting ${count}`;
    case "uploading":
      return `Uploading ${count}${file}`;
    case "downloading":
      return `Downloading ${count}${file}`;
    default:
      return `Working ${count}`;
  }
}

// MAJOR.MINOR.PATCH with no leading zeros, matching parse_semver in publish.rs
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

const VERSION_HELP = [
  "Version to publish, as MAJOR.MINOR.PATCH:",
  "- PATCH (1.4.2 to 1.4.3): fixes, mod updates, addon or setting tweaks",
  "- MINOR (1.4.2 to 1.5.0): mods added, nothing removed",
  "- MAJOR (1.4.2 to 2.0.0): mods removed or swapped, players may need to redo their setup",
].join("\n");

// Next patch after the current version. Older versions like "1.5" are padded first.
function suggestVersion(current: string | null | undefined): string {
  const parts = (current ?? "").trim().split(".").map((p) => (/^\d+$/.test(p) ? Number(p) : NaN));
  if (parts.length === 0 || parts.length > 3 || parts.some(Number.isNaN)) return "1.0.0";
  while (parts.length < 3) parts.push(0);
  return `${parts[0]}.${parts[1]}.${parts[2] + 1}`;
}

function askVersion(pack: PackManifest): string | null {
  let value = suggestVersion(pack.version);
  let message = VERSION_HELP;
  for (;;) {
    const input = prompt(message, value);
    if (input === null) return null;
    const version = input.trim();
    if (SEMVER.test(version)) return version;
    value = version;
    message = `${version || "That"} is not a valid version. Use MAJOR.MINOR.PATCH, for example 1.4.2.\n\n${VERSION_HELP}`;
  }
}

export async function publishPack(pack: PackManifest): Promise<boolean> {
  const version = askVersion(pack);
  if (version === null) return false;
  let outcome: PublishOutcome;
  beginOp(pack.pack_id);
  try {
    outcome = await invoke("publish_pack_cmd", { packId: pack.pack_id, version }) as PublishOutcome;
  } catch (e) {
    console.error("Failed to publish pack:", e);
    alert("Failed to publish: " + e);
    return false;
  } finally {
    endOp(pack.pack_id);
    await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
  }
  if (outcome.kind === "Published") {
    await copyText(folderLink(outcome.folder_id));
    alert(`Published ${pack.name} ${outcome.version}. The link is copied, share it with your players.`);
  } else {
    alert("Nothing to publish");
  }
  return true;
}
