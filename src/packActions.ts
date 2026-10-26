import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  store, type ModRef, type PackManifest, type PackProgress, type PackRef, type ServerStatus, type ServerUser, type UpdateCheck
} from "./store";
import { showAlert, showChoice, showConfirm, showCopy, showPrompt } from "./dialogs";

type PublishOutcome =
  | { kind: "Published"; server: string; version: string; uploaded: number; reused: number }
  | { kind: "NothingToPublish"; server: string };

interface InvitePack {
  id: string;
  name: string;
  version: string;
  uploader: ServerUser;
}

interface LinkInfo {
  base: string;
  code: string;
  kind: string;
  pack: InvitePack | null;
  signed_in: boolean;
  installed: { version: string | null; from: string } | null;
}

type AddOutcome =
  | { kind: "uploader"; base: string }
  | { kind: "installed"; pack_id: string; version: string };

export interface ServerLink {
  url: string;
  expires_at: number;
}

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

export async function loadServers(): Promise<void> {
  try {
    store.servers = await invoke("list_servers") as ServerStatus[];
  } catch (e) {
    console.error("Failed to load pack servers:", e);
  }
}

export function hostOf(base: string): string {
  try {
    return new URL(base).host;
  } catch {
    return base;
  }
}

function canUpload(s: ServerStatus): boolean {
  return s.user?.role === "uploader" || s.user?.role === "owner";
}

export function canPublish(): boolean {
  return store.servers.some(canUpload);
}

// A signed out error means the server dropped the session, so the server list is stale
export function noteSignedOut(e: unknown): void {
  if (String(e).includes("Signed out of")) loadServers();
}

export async function alertFailure(prefix: string, e: unknown): Promise<void> {
  noteSignedOut(e);
  await showAlert(prefix + e);
}

export const SIGN_IN_PREFIX = "sign-in:";

// Sign-in waits for the browser redirect for up to 300 s, so it shows as an op in the progress modal
export async function signInServer(base: string): Promise<ServerUser> {
  const key = SIGN_IN_PREFIX + base;
  if (store.inFlight[key]) throw `Already signing in to ${hostOf(base)}, finish in your browser first`;
  beginOp(key, `Sign in to ${hostOf(base)}`);
  store.packProgress[key] = {
    pack_id: key, stage: "signing_in", done: 0, total: 0, file: null,
    bytes_done: 0, bytes_total: 0, overall_done: 0, overall_total: 0, wait_secs: 0,
  };
  try {
    return await invoke("sign_in_server", { base }) as ServerUser;
  } finally {
    endOp(key);
    await loadServers();
  }
}

export async function signOutServer(base: string): Promise<void> {
  try {
    await invoke("sign_out_server", { base });
  } finally {
    await loadServers();
  }
}

export async function inviteLink(base: string, packId: string): Promise<string> {
  const link = await invoke("pack_invite_link", { base, packId }) as ServerLink;
  return link.url;
}

export async function copyInviteLink(pack: PackManifest): Promise<void> {
  const base = pack.published?.server;
  if (!base) return;
  try {
    await copyText(await inviteLink(base, pack.pack_id));
  } catch (e) {
    console.error("Failed to copy the invite link:", e);
    await alertFailure("Failed to copy the invite link: ", e);
  }
}

export async function leavePack(pack: PackManifest): Promise<void> {
  if (store.inFlight[pack.pack_id]) return;
  const question = `Leave ${pack.name}? It stays installed as a local pack and stops getting updates.`;
  if (!(await showConfirm(question, { danger: true, confirmLabel: "Leave" }))) return;
  if (store.inFlight[pack.pack_id]) return;
  try {
    await invoke("leave_pack", { packId: pack.pack_id });
  } catch (e) {
    console.error("Failed to leave the pack:", e);
    await alertFailure("Failed to leave the pack: ", e);
  }
  await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
  delete store.packUpdates[pack.pack_id];
}

export function isLinked(packId: string): boolean {
  return !!store.packs.find((p) => p.pack_id === packId)?.source;
}

export function isPublished(packId: string): boolean {
  return !!store.packs.find((p) => p.pack_id === packId)?.published?.server;
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
  delete store.cancelling[packId];
}

// The error a download or upload op rejects with when the player cancels it, matching server::CANCELLED
export const CANCELLED = "Cancelled";

export function opName(packId: string): string {
  return store.packs.find((p) => p.pack_id === packId)?.name ?? store.opNames[packId] ?? packId;
}

const UPLOAD_STAGES = ["uploading", "checking_server"];
const CANCEL_STAGES = ["downloading", "waiting", ...UPLOAD_STAGES];

// Only transfers can be cancelled, and only while they are still moving files or waiting to retry
export function canCancel(packId: string): boolean {
  const stage = store.packProgress[packId]?.stage ?? "";
  return !!store.inFlight[packId] && CANCEL_STAGES.includes(stage);
}

export async function cancelPackOp(packId: string): Promise<void> {
  if (store.cancelling[packId] || !canCancel(packId)) return;
  const upload = UPLOAD_STAGES.includes(store.packProgress[packId]?.stage ?? "");
  const confirmed = await showConfirm(
    upload
      ? `Cancel publishing ${opName(packId)}? The version already on the server stays as it is.`
      : `Cancel downloading ${opName(packId)}? Nothing will be changed.`,
    {
      confirmLabel: upload ? "Cancel publish" : "Cancel download",
      cancelLabel: upload ? "Keep publishing" : "Keep downloading",
      danger: true,
    },
  );
  if (!confirmed || store.cancelling[packId] || !canCancel(packId)) return;
  store.cancelling[packId] = true;
  try {
    await invoke("cancel_pack_op", { packId });
  } catch (e) {
    console.error("Failed to cancel:", e);
    delete store.cancelling[packId];
  }
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
    case "changelog":
      return "Writing the changelog";
    case "checking_server":
      return p.total > 1 ? `Checking the pack server ${count}` : "Checking the pack server";
    case "signing_in":
      return "Waiting for Discord sign-in in your browser";
    case "deleting":
      return `Deleting ${count}`;
    case "uploading":
      return `Uploading ${count}${file}`;
    case "downloading":
      return `Downloading ${count}${file}`;
    case "waiting":
      return `The pack server asked to wait, retrying in ${p.wait_secs}s`;
    case "extracting":
      return `Extracting ${count}${file}`;
    case "installing":
      return "Installing";
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

// Matches publish::CHOOSE_SERVER, the error when several servers could take a new pack
const CHOOSE_SERVER = "choose_server";

async function chooseServer(pack: PackManifest): Promise<string | null> {
  const choices = store.servers.filter(canUpload).map((s) => ({ label: hostOf(s.base), value: s.base }));
  if (choices.length === 0) {
    await showAlert("Sign in to a pack server where you can upload first.");
    return null;
  }
  return showChoice(`Which pack server should ${pack.name} be published to?`, choices, { title: `Publish ${pack.name}` });
}

export async function publishPack(pack: PackManifest): Promise<boolean> {
  const version = await askVersion(pack);
  if (version === null) return false;
  let server: string | null = null;
  for (;;) {
    let outcome: PublishOutcome | null = null;
    let failed: { error: unknown } | null = null;
    beginOp(pack.pack_id, pack.name);
    try {
      outcome = await invoke("publish_pack_cmd", { packId: pack.pack_id, version, server }) as PublishOutcome;
    } catch (e) {
      if (e !== CHOOSE_SERVER) console.error("Failed to publish pack:", e);
      failed = { error: e };
    } finally {
      endOp(pack.pack_id);
      await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
    }
    if (failed?.error === CHOOSE_SERVER && server === null) {
      await loadServers();
      server = await chooseServer(pack);
      if (server === null) return false;
      continue;
    }
    if (failed !== null || outcome === null) {
      if (failed?.error !== CANCELLED) await alertFailure("Failed to publish: ", failed?.error);
      return false;
    }
    if (outcome.kind === "Published") {
      const copy = await showConfirm(`Published ${pack.name} ${outcome.version}. Share the invite link with your players.`, {
        confirmLabel: "Copy invite link",
        cancelLabel: "Close",
      });
      if (copy) {
        try {
          await copyText(await inviteLink(outcome.server, pack.pack_id));
        } catch (e) {
          console.error("Failed to copy the invite link:", e);
          await alertFailure("Failed to copy the invite link: ", e);
        }
      }
    } else {
      await showAlert("Nothing to publish");
    }
    return true;
  }
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
async function finishImport(added: PackRef, failedDeletes: string[], imported: { label: string }): Promise<void> {
  imported.label = packLabel(added.name, added.version);
  await loadPresets();
  await loadPacks();
  const pack = store.packs.find((p) => p.pack_id === added.pack_id);
  if (pack) await applyPack(pack);
  else await refreshMods();

  let message = `${imported.label} installed${pack ? " and active" : ""}.`;
  if (failedDeletes.length > 0) {
    message += `\nThese folders could not be deleted: ${failedDeletes.join(", ")}`;
  }
  await showAlert(message);
}

export async function importArchive(archivePath: string): Promise<void> {
  const imported = { label: "" };
  try {
    await loadPacks();
    const preview = await invoke("inspect_preset_archive", { archivePath }) as ImportPreview;
    const busy = `${preview.pack.name} is busy, try again when it finishes`;
    if (store.inFlight[preview.pack.pack_id]) {
      await showAlert(busy);
      return;
    }
    const installed = store.packs.find((p) => p.pack_id === preview.pack.pack_id);
    const question = importQuestion(preview, installed);
    if (question && !(await showConfirm(question))) return;
    if (store.inFlight[preview.pack.pack_id]) {
      await showAlert(busy);
      return;
    }

    const packId = preview.pack.pack_id;
    beginOp(packId, preview.pack.name);
    let summary: ImportSummary;
    try {
      summary = await invoke("import_preset", { archivePath }) as ImportSummary;
    } finally {
      endOp(packId);
    }
    await finishImport(summary.pack, summary.failed_deletes, imported);
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
  const seq = ++updateCheckSeq;
  try {
    const checks = await invoke("check_pack_updates") as UpdateCheck[];
    if (seq !== updateCheckSeq) return;
    store.packUpdates = Object.fromEntries(checks.map((c) => [c.pack_id, c]));
    // The check also refreshes each server's stored role and drops expired sessions
    await loadServers();
  } catch (e) {
    console.error("Failed to check modpack updates:", e);
  }
}

export function updateLine(check: UpdateCheck): string {
  return `Update available: ${check.installed_version ?? "none"} to ${check.remote_version ?? "none"}`;
}

// The line a pack card shows for its update check, or "" when there is nothing to say
export function updateStatusLine(check: UpdateCheck | undefined): string {
  if (!check) return "";
  if (check.status === "available") return updateLine(check);
  if (check.status === "error" || check.status === "unsupported") return check.error ?? "";
  return "";
}

// switchFrom is the pack a switch replaces, which may hold the same id from another server
async function inspectLink(link: string, switchFrom?: string): Promise<LinkInfo> {
  return await invoke("inspect_pack_link", { link, switchFrom: switchFrom ?? null }) as LinkInfo;
}

async function acceptUploaderInvite(link: string, info: LinkInfo): Promise<void> {
  const host = hostOf(info.base);
  if (!(await showConfirm(`Become an uploader on ${host}?`))) return;
  await invoke("add_pack_from_link", { link }) as AddOutcome;
  await loadServers();
  await showAlert(`You can now publish packs to ${host}.`);
}

export async function addPackFromLink(): Promise<void> {
  const input = await showPrompt("Paste the invite link:", "", { title: "Add Modpack" });
  if (!input || !input.trim()) return;
  const link = input.trim();
  const imported = { label: "" };
  try {
    await loadPacks();
    let info = await inspectLink(link);
    if (!info.signed_in) {
      if (!(await showConfirm(`Sign in to ${hostOf(info.base)} with Discord to continue?`))) return;
      await signInServer(info.base);
      checkPackUpdates();
      info = await inspectLink(link);
      if (!info.signed_in) return;
    }
    if (info.kind === "uploader") {
      await acceptUploaderInvite(link, info);
      return;
    }
    if (!info.pack) throw new Error("That invite has no pack");
    const { id: packId, name, version } = info.pack;
    const busy = `${name} is busy, try again when it finishes`;
    if (store.inFlight[packId]) {
      await showAlert(busy);
      return;
    }
    const installed = store.packs.find((p) => p.pack_id === packId);
    let question = `Install ${packLabel(name, version)}?`;
    if (installed?.version === version) {
      question = `Reinstall ${packLabel(name, version)}?`;
    } else if (installed) {
      const from = installed.version ? ` from ${installed.version}` : "";
      question = `Update ${name}${from} to ${version}?`;
    }
    if (!(await showConfirm(question))) return;
    if (store.inFlight[packId]) {
      await showAlert(busy);
      return;
    }

    beginOp(packId, name);
    let outcome: AddOutcome;
    try {
      outcome = await invoke("add_pack_from_link", { link }) as AddOutcome;
    } finally {
      endOp(packId);
    }
    if (outcome.kind === "installed") {
      await finishImport({ pack_id: outcome.pack_id, name, version: outcome.version || null }, [], imported);
    }
  } catch (e: any) {
    if (imported.label) {
      console.error("Add from link follow-up failed:", e);
      await showAlert(`${imported.label} was imported, but the next step failed: ${e}`);
    } else if (e !== CANCELLED) {
      console.error("Failed to add the modpack:", e);
      await alertFailure("Failed to add the modpack: ", e);
    }
  }
  if (imported.label) await checkPackUpdates();
}

interface SwitchPreview {
  base: string;
  new_pack_id: string;
  name: string;
  version: string;
  uploader: string | null;
  reused_files: number;
  total_files: number;
  download_bytes: number;
}

interface SwitchSummary extends ImportSummary {
  not_enabled: string[];
}

// Held from the first prompt to the end, so a double click cannot stack prompts or switch twice
let switching = false;

export async function switchPackLink(pack: PackManifest): Promise<void> {
  if (switching || store.inFlight[pack.pack_id] || pack.published?.server) return;
  switching = true;
  try {
    await runSwitch(pack);
  } finally {
    switching = false;
  }
}

async function runSwitch(pack: PackManifest): Promise<void> {
  const oldId = pack.pack_id;
  const input = await showPrompt("Paste the invite link to switch to:", "", { title: `Switch ${pack.name}` });
  if (!input || !input.trim()) return;
  const link = input.trim();
  const busy = (name: string) => `${name} is busy, try again when it finishes`;
  try {
    const info = await inspectLink(link, oldId);
    if (!info.signed_in) {
      if (!(await showConfirm(`Sign in to ${hostOf(info.base)} with Discord to continue?`))) return;
      await signInServer(info.base);
      checkPackUpdates();
      if (!(await inspectLink(link, oldId)).signed_in) return;
    }
    if (store.inFlight[oldId]) {
      await showAlert(busy(pack.name));
      return;
    }
    const preview = await invoke("preview_switch", { packId: oldId, link }) as SwitchPreview;
    const newId = preview.new_pack_id;
    const by = preview.uploader ? ` by ${preview.uploader}` : "";
    const question = `Switch ${pack.name} to ${preview.name}${by}? ${preview.reused_files} of ${preview.total_files} files reused, ${formatSize(preview.download_bytes)} to download.`;
    if (!(await showConfirm(question, { confirmLabel: "Switch" }))) return;
    if (store.inFlight[oldId] || store.inFlight[newId]) {
      await showAlert(busy(store.inFlight[oldId] ? pack.name : preview.name));
      return;
    }

    let summary: SwitchSummary | null = null;
    let failed: { error: unknown } | null = null;
    // Download progress is reported under the new pack id, the old one only stays locked
    beginOp(newId, preview.name);
    if (newId !== oldId) {
      beginOp(oldId, pack.name);
      store.progressHidden[oldId] = true;
    }
    try {
      summary = await invoke("switch_pack", { packId: oldId, base: preview.base, newPackId: newId, link }) as SwitchSummary;
    } catch (e) {
      if (e !== CANCELLED) {
        console.error("Failed to switch:", e);
        failed = { error: e };
      }
    }
    endOp(newId);
    endOp(oldId);
    if (summary && store.activePresetName === `pack:${oldId}`) store.activePresetName = `pack:${newId}`;
    await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
    await loadPresets().catch((e) => console.error("Failed to load presets:", e));
    if (store.selectedPack === oldId && !store.packs.some((p) => p.pack_id === oldId)) store.selectedPack = null;
    await checkPackUpdates();
    if (failed !== null) {
      await alertFailure("Failed to switch: ", failed.error);
      return;
    }
    if (summary === null) return;
    if (summary.failed_deletes.length > 0) {
      console.warn(`Folders left behind after switching ${pack.name}:`, summary.failed_deletes);
    }
    let message = `Switched ${pack.name} to ${packLabel(summary.pack.name, summary.pack.version)}.`;
    if (summary.not_enabled.length > 0) {
      message += ` These mods could not be turned on: ${summary.not_enabled.join(", ")}`;
    }
    await showAlert(message);
  } catch (e) {
    if (e !== CANCELLED) {
      console.error("Failed to switch:", e);
      await alertFailure("Failed to switch: ", e);
    }
  }
}

async function runPackUpdate(pack: PackManifest, run: () => Promise<UpdateSummary>, failure: string): Promise<void> {
  let summary: UpdateSummary | null = null;
  let failed: { error: unknown } | null = null;
  beginOp(pack.pack_id, pack.name);
  try {
    summary = await run();
  } catch (e) {
    if (e !== CANCELLED) {
      console.error(failure, e);
      failed = { error: e };
    }
  }
  endOp(pack.pack_id);
  if (failed !== null) await alertFailure(failure, failed.error);
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

// Resolves true when the player picks Update. A history view always resolves false.
export function openPackChangelog(packId: string, mode: "history" | "update", summary = ""): Promise<boolean> {
  store.packChangelog?.resolve(false);
  return new Promise((resolve) => {
    store.packChangelog = { packId, mode, summary, resolve };
  });
}

export function closePackChangelog(update: boolean): void {
  const request = store.packChangelog;
  store.packChangelog = null;
  request?.resolve(update);
}

export async function updatePack(pack: PackManifest): Promise<void> {
  const check = store.packUpdates[pack.pack_id];
  if (check?.status !== "available" || store.inFlight[pack.pack_id]) return;
  const summary = `${check.added.length} mods added, ${check.changed.length} updated, ${check.removed.length} removed and deleted from disk. Downloads ${formatSize(check.download_bytes)}.`;
  if (!(await openPackChangelog(pack.pack_id, "update", summary))) return;
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
