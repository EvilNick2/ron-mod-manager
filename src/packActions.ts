import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { store, type ModRef, type PackManifest, type PackProgress, type PackRef, type UpdateCheck } from "./store";
import { showAlert, showConfirm, showCopy, showPrompt } from "./dialogs";

type PublishOutcome =
  | { kind: "Published"; folder_id: string; version: string; uploaded: number; deleted: number }
  | { kind: "NothingToPublish"; folder_id: string };

export interface ImportPreview {
  pack: PackRef;
  installed_version: string | null;
  added: ModRef[];
  removed: ModRef[];
  changed: ModRef[];
  superseded: ModRef[];
}

export interface ImportSummary {
  pack: PackRef;
  keys: string[];
  added: number;
  changed: number;
  removed_keys: string[];
  superseded_ids: string[];
  failed_deletes: string[];
}

interface UpdateSummary {
  pack: PackRef;
  downloaded: number;
  added: number;
  changed: number;
  removed_keys: string[];
  failed_deletes: string[];
  not_enabled: string[];
}

export async function loadPresets(): Promise<void> {
  try {
    store.presets = await invoke("load_presets") as Record<string, string[]>;
    const active = store.activePresetName;
    if (active && !active.startsWith("pack:") && !store.presets[active]) {
      store.activePresetName = "";
    }
  } catch (e) {
    console.error("Failed to load presets:", e);
  }
}

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
    await showCopy(text);
  }
}

export function beginOp(packId: string, name?: string): void {
  store.inFlight[packId] = true;
  if (name) store.opNames[packId] = name;
}

export function endOp(packId: string): void {
  delete store.inFlight[packId];
  delete store.packProgress[packId];
  delete store.opNames[packId];
  delete store.progressHidden[packId];
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

export function hasVisibleOps(): boolean {
  return !store.closeBlocked && Object.keys(store.inFlight).some((id) => !store.progressHidden[id]);
}

// Bytes across the whole stage when it moves bytes, else items done
export function progressPercent(p: PackProgress): number {
  if (p.overall_total > 0) return Math.min(100, (p.overall_done / p.overall_total) * 100);
  return p.total > 0 ? Math.min(100, (p.done / p.total) * 100) : 0;
}

export function progressAmount(p: PackProgress): string {
  return p.overall_total > 0 ? `${formatSize(p.overall_done)} of ${formatSize(p.overall_total)}` : "";
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

async function askVersion(pack: PackManifest): Promise<string | null> {
  const input = await showPrompt(VERSION_HELP, suggestVersion(pack.version), {
    title: `Publish ${pack.name}`,
    confirmLabel: "Publish",
    validate: (v) => {
      const version = v.trim();
      return SEMVER.test(version) ? null : `${version || "That"} is not a valid version. Use MAJOR.MINOR.PATCH, for example 1.4.2.`;
    },
  });
  return input === null ? null : input.trim();
}

export async function publishPack(pack: PackManifest): Promise<boolean> {
  const version = await askVersion(pack);
  if (version === null) return false;
  let outcome: PublishOutcome | null = null;
  let failed: { error: unknown } | null = null;
  beginOp(pack.pack_id, pack.name);
  try {
    outcome = await invoke("publish_pack_cmd", { packId: pack.pack_id, version }) as PublishOutcome;
  } catch (e) {
    console.error("Failed to publish pack:", e);
    failed = { error: e };
  } finally {
    endOp(pack.pack_id);
    await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
  }
  if (failed !== null || outcome === null) {
    await showAlert("Failed to publish: " + failed?.error);
    return false;
  }
  if (outcome.kind === "Published") {
    await copyText(folderLink(outcome.folder_id));
    await showAlert(`Published ${pack.name} ${outcome.version}. The link is copied, share it with your players.`);
  } else {
    await showAlert("Nothing to publish");
  }
  return true;
}

export function packLabel(name: string, version: string | null): string {
  return version ? `${name} ${version}` : name;
}

export function nameList(names: string[]): string {
  const shown = names.slice(0, 10).join(", ");
  return names.length > 10 ? `${shown} and ${names.length - 10} more` : shown;
}

// The confirm text for importing a pack, or "" when nothing on disk is replaced
export function importQuestion(preview: ImportPreview, installed: PackManifest | undefined): string {
  const { name, version } = preview.pack;
  const { removed, superseded } = preview;
  if (removed.length + superseded.length === 0) return "";
  const supersededText = `${superseded.length} installed mods are also in this pack and their old copies will be deleted from disk.`;
  if (!installed) return `Import ${packLabel(name, version)}? ${supersededText}`;
  const from = installed.version ? ` from ${installed.version}` : "";
  const to = version ? ` to ${version}` : "";
  let question = `Update ${name}${from}${to}? ${preview.added.length} mods added, ${preview.changed.length} updated`;
  question += removed.length > 0
    ? `, ${removed.length} removed and deleted from disk: ${nameList(removed.map((m) => m.name))}.`
    : ".";
  if (superseded.length > 0) question += ` ${supersededText}`;
  return question;
}

// Applies a freshly imported pack and tells the player. Sets the label first so a caller can name the pack in errors.
async function finishImport(summary: ImportSummary, imported: { label: string }): Promise<void> {
  imported.label = packLabel(summary.pack.name, summary.pack.version);
  await loadPresets();
  await loadPacks();
  const pack = store.packs.find((p) => p.pack_id === summary.pack.pack_id);
  if (pack) await applyPack(pack);
  else await refreshMods();

  let message = `${imported.label} installed${pack ? " and active" : ""}.`;
  if (summary.failed_deletes.length > 0) {
    message += `\nThese folders could not be deleted: ${summary.failed_deletes.join(", ")}`;
  }
  await showAlert(message);
}

export async function importArchive(archivePath: string): Promise<void> {
  const imported = { label: "" };
  try {
    await loadPacks();
    const preview = await invoke("inspect_preset_archive", { archivePath }) as ImportPreview;
    const installed = store.packs.find((p) => p.pack_id === preview.pack.pack_id);
    const question = importQuestion(preview, installed);
    if (question && !(await showConfirm(question))) return;

    const summary = await invoke("import_preset", { archivePath }) as ImportSummary;
    await finishImport(summary, imported);
  } catch (e: any) {
    if (imported.label) {
      console.error("Import follow-up failed:", e);
      await showAlert(`${imported.label} was imported, but the next step failed: ${e}`);
    } else if (e !== "Import cancelled") {
      console.error("Failed to import preset:", e);
      await showAlert("Failed to import preset: " + e);
    }
  }
  if (imported.label) await checkPackUpdates();
}

let updateCheckSeq = 0;

export async function checkPackUpdates(): Promise<void> {
  if (!store.google.linksAvailable) return;
  const seq = ++updateCheckSeq;
  try {
    const checks = await invoke("check_pack_updates") as UpdateCheck[];
    if (seq !== updateCheckSeq) return;
    store.packUpdates = Object.fromEntries(checks.map((c) => [c.pack_id, c]));
  } catch (e) {
    console.error("Failed to check modpack updates:", e);
  }
}

export function updateLine(check: UpdateCheck): string {
  return `Update available: ${check.installed_version ?? "none"} to ${check.remote_version ?? "none"}`;
}

export async function addPackFromLink(): Promise<void> {
  const link = await showPrompt("Paste the modpack's Google Drive link:", "", { title: "Add Modpack" });
  if (!link || !link.trim()) return;
  const imported = { label: "" };
  try {
    await loadPacks();
    const result = await invoke("inspect_pack_link", { link }) as { preview: ImportPreview; download_bytes: number };
    const preview = result.preview;
    const { name, version } = preview.pack;
    if (store.inFlight[preview.pack.pack_id]) {
      await showAlert(`${name} is busy, try again when it finishes`);
      return;
    }
    const installed = store.packs.find((p) => p.pack_id === preview.pack.pack_id);
    let question = importQuestion(preview, installed);
    if (!question && installed) {
      const from = installed.version ? ` from ${installed.version}` : "";
      const to = version ? ` to ${version}` : "";
      question = `Update ${name}${from}${to}?`;
    }
    if (!question) question = `Import ${packLabel(name, version)}?`;
    if (!(await showConfirm(`${question} Downloads ${formatSize(result.download_bytes)}.`))) return;

    const packId = preview.pack.pack_id;
    if (store.inFlight[packId]) {
      await showAlert(`${name} is busy, try again when it finishes`);
      return;
    }
    beginOp(packId, name);
    let summary: ImportSummary;
    try {
      summary = await invoke("add_pack_from_link", { link }) as ImportSummary;
    } finally {
      endOp(packId);
    }
    await finishImport(summary, imported);
  } catch (e: any) {
    if (imported.label) {
      console.error("Add from link follow-up failed:", e);
      await showAlert(`${imported.label} was imported, but the next step failed: ${e}`);
    } else {
      console.error("Failed to add the modpack:", e);
      await showAlert("Failed to add the modpack: " + e);
    }
  }
  if (imported.label) await checkPackUpdates();
}

async function runPackUpdate(pack: PackManifest, run: () => Promise<UpdateSummary>, failure: string): Promise<void> {
  let summary: UpdateSummary | null = null;
  let failed: { error: unknown } | null = null;
  beginOp(pack.pack_id, pack.name);
  try {
    summary = await run();
  } catch (e) {
    console.error(failure, e);
    failed = { error: e };
  }
  endOp(pack.pack_id);
  if (failed !== null) await showAlert(failure + failed.error);
  await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
  await loadPresets().catch((e) => console.error("Failed to load presets:", e));
  await checkPackUpdates();
  if (summary === null) return;
  if (summary.failed_deletes.length > 0) {
    console.warn(`Folders left behind after updating ${pack.name}:`, summary.failed_deletes);
  }
  if (summary.not_enabled.length > 0) {
    await showAlert(`${pack.name} was updated, but these mods could not be turned on: ${summary.not_enabled.join(", ")}`);
  }
}

export async function updatePack(pack: PackManifest): Promise<void> {
  const check = store.packUpdates[pack.pack_id];
  if (check?.status !== "available" || store.inFlight[pack.pack_id]) return;
  const question = `Update ${pack.name} to ${check.remote_version}? ${check.added.length} mods added, ${check.changed.length} updated, ${check.removed.length} removed and deleted from disk. Downloads ${formatSize(check.download_bytes)}.`;
  if (!(await showConfirm(question))) return;
  if (store.inFlight[pack.pack_id]) return;
  await runPackUpdate(pack, () => invoke("update_linked_pack", { packId: pack.pack_id }), "Failed to update: ");
}

export async function verifyPack(pack: PackManifest): Promise<void> {
  if (store.inFlight[pack.pack_id]) return;
  let broken: string[];
  beginOp(pack.pack_id, pack.name);
  try {
    broken = await invoke("verify_linked_pack", { packId: pack.pack_id }) as string[];
  } catch (e) {
    console.error("Failed to verify:", e);
    endOp(pack.pack_id);
    await showAlert("Failed to verify: " + e);
    return;
  }
  endOp(pack.pack_id);
  if (broken.length === 0) {
    await showAlert("All files match.");
    return;
  }
  const check = store.packUpdates[pack.pack_id];
  const question = check?.status === "available"
    ? `Redownload ${broken.length} mods? This also installs the update to ${check.remote_version}.`
    : `Redownload ${broken.length} mods?`;
  if (!(await showConfirm(question))) return;
  if (store.inFlight[pack.pack_id]) return;
  await runPackUpdate(
    pack,
    () => invoke("redownload_mods", { packId: pack.pack_id, modIds: broken }),
    "Failed to verify: ",
  );
}

export async function forkPack(pack: PackManifest): Promise<void> {
  if (store.inFlight[pack.pack_id]) return;
  let size: number;
  try {
    size = await invoke("get_pack_size", { packId: pack.pack_id }) as number;
  } catch (e) {
    console.error("Failed to fork:", e);
    await showAlert("Failed to fork: " + e);
    return;
  }
  const name = await showPrompt("Name for the fork:", `${pack.name} (fork)`, { title: `Fork ${pack.name}` });
  if (name === null || store.inFlight[pack.pack_id]) return;
  if (!(await showConfirm(`Forking copies ${formatSize(size)} of mods. Continue?`))) return;
  if (store.inFlight[pack.pack_id]) return;
  let failed: { error: unknown } | null = null;
  beginOp(pack.pack_id, pack.name);
  try {
    await invoke("fork_pack", { packId: pack.pack_id, name });
  } catch (e) {
    console.error("Failed to fork:", e);
    failed = { error: e };
  } finally {
    endOp(pack.pack_id);
    await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
  }
  if (failed !== null) await showAlert("Failed to fork: " + failed.error);
}
