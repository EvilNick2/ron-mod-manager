<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, ref, watch } from "vue";
import { Copy, FileText, Link, Repeat, Users } from "lucide-vue-next";
import { store } from "../store";
import {
  copyInviteLink, copyText, formatSize, hostOf, openPackChangelog, switchPackLink, updateLine
} from "../packActions";

// Matches subscribe::DRIVE_GONE, shown before the first update check comes back
const DRIVE_GONE = "Google Drive packs are no longer supported. Ask the pack's uploader for a new invite link.";

const pack = computed(() => store.packs.find((p) => p.pack_id === store.selectedPack) ?? null);
const fromServer = computed(() => pack.value?.source?.kind === "server");
const fromDrive = computed(() => !!pack.value?.source && !fromServer.value);
const publishedTo = computed(() => pack.value?.published?.server ?? null);
const check = computed(() => (pack.value ? store.packUpdates[pack.value.pack_id] : undefined));
const busy = computed(() => !!(pack.value && store.inFlight[pack.value.pack_id]));

const size = ref<number | null>(null);
const sizeError = ref(false);
const hasSavedChangelog = ref(false);

const installedDate = computed(() => {
  const at = pack.value?.imported_at ?? 0;
  return at > 0 ? new Date(at * 1000).toLocaleDateString() : "";
});

const checkText = computed(() => {
  const c = check.value;
  if (!c) return "";
  if (c.status === "current") return "Up to date";
  if (c.status === "available") return updateLine(c);
  return c.error ?? "";
});

const driveText = computed(() => (check.value?.status === "unsupported" && check.value.error) || DRIVE_GONE);

// A server pack can always show its history, other packs only when they carry a changelog.json
const canShowChangelog = computed(() => fromServer.value || hasSavedChangelog.value);

// The pack disappears when it is deleted or switched to another pack
watch(pack, (p) => {
  if (!p) store.selectedPack = null;
});

let sizeSeq = 0;

async function loadSize(packId: string) {
  const seq = ++sizeSeq;
  size.value = null;
  sizeError.value = false;
  try {
    const bytes = await invoke("get_pack_size", { packId }) as number;
    if (seq === sizeSeq) size.value = bytes;
  } catch (e) {
    console.error("Failed to get the pack size:", e);
    if (seq === sizeSeq) sizeError.value = true;
  }
}

let changelogSeq = 0;

// Without a pack server this only reads the saved file, so it is cheap
async function loadSavedChangelog(packId: string) {
  const seq = ++changelogSeq;
  hasSavedChangelog.value = false;
  if (fromServer.value) return;
  try {
    const changelog = await invoke("pack_changelog", { packId }) as { entries: unknown[] };
    if (seq === changelogSeq) hasSavedChangelog.value = changelog.entries.length > 0;
  } catch (e) {
    console.error("Failed to read the changelog:", e);
  }
}

watch(
  () => pack.value ? [pack.value.pack_id, pack.value.version, pack.value.mod_ids.length, pack.value.published?.server ?? ""].join("|") : "",
  (key) => {
    if (!key || !pack.value) return;
    loadSize(pack.value.pack_id);
    loadSavedChangelog(pack.value.pack_id);
  },
  { immediate: true },
);

async function copySourceInvite() {
  const invite = pack.value?.source?.invite;
  if (invite) await copyText(invite);
}

function showChangelog() {
  if (pack.value) openPackChangelog(pack.value.pack_id, "history");
}

function switchLink() {
  if (pack.value) switchPackLink(pack.value);
}

function manageAccess() {
  if (pack.value) store.packAccessFor = pack.value.pack_id;
}
</script>

<template>
  <aside v-if="pack" class="right-panel">
    <div class="panel-header">
      <h2>{{ pack.name }}</h2>
    </div>

    <div class="detail-content">
      <div class="detail-row" v-if="pack.version">
        <span class="detail-label">Version</span>
        <span class="detail-value">{{ pack.version }}</span>
      </div>
      <div class="detail-row">
        <span class="detail-label">Mods</span>
        <span class="detail-value">{{ pack.mod_ids.length }}</span>
      </div>
      <div class="detail-row">
        <span class="detail-label">Size on disk</span>
        <span class="detail-value">{{ sizeError ? "Unknown" : size === null ? "Calculating..." : formatSize(size) }}</span>
      </div>
      <div class="detail-row" v-if="installedDate">
        <span class="detail-label">Installed</span>
        <span class="detail-value">{{ installedDate }}</span>
      </div>

      <template v-if="fromServer">
        <div class="detail-row" v-if="pack.source?.server">
          <span class="detail-label">Server</span>
          <span class="detail-value">{{ hostOf(pack.source.server) }}</span>
        </div>
        <div class="detail-row" v-if="pack.source?.uploader">
          <span class="detail-label">Uploader</span>
          <span class="detail-value">{{ pack.source.uploader }}</span>
        </div>
        <div class="detail-block" v-if="pack.source?.invite">
          <span class="detail-label">Invite link</span>
          <div class="invite-row">
            <span class="invite-text" :title="pack.source.invite">{{ pack.source.invite }}</span>
            <button class="icon-btn" @click="copySourceInvite" title="Copy invite link">
              <Copy :size="14" color="#a1a1aa" />
            </button>
          </div>
        </div>
        <p v-if="checkText" class="detail-note" :class="{ 'is-error': check?.status === 'error' }">{{ checkText }}</p>
      </template>

      <p v-else-if="fromDrive" class="detail-note is-error">{{ driveText }}</p>

      <template v-if="publishedTo">
        <div class="detail-row">
          <span class="detail-label">Server</span>
          <span class="detail-value">{{ hostOf(publishedTo) }}</span>
        </div>
        <div class="detail-row" v-if="pack.version">
          <span class="detail-label">Published version</span>
          <span class="detail-value">{{ pack.version }}</span>
        </div>
        <span v-if="pack.published?.dirty" class="pack-dirty">Changes not uploaded</span>
      </template>

      <div class="action-list">
        <template v-if="publishedTo">
          <button class="pack-btn" @click="copyInviteLink(pack)">
            <Link :size="15" color="#a1a1aa" /> Copy invite link
          </button>
          <button class="pack-btn" @click="manageAccess">
            <Users :size="15" color="#a1a1aa" /> Manage access
          </button>
        </template>
        <button v-if="canShowChangelog" class="pack-btn" @click="showChangelog">
          <FileText :size="15" color="#a1a1aa" /> Changelog
        </button>
        <button v-if="!publishedTo" class="pack-btn" @click="switchLink" :disabled="busy">
          <Repeat :size="15" color="#a1a1aa" /> Switch link
        </button>
      </div>
    </div>
  </aside>
</template>

<style scoped>
.right-panel {
  background-color: var(--bg-dark);
  border-left: 1px solid var(--border-dark);
  display: flex;
  flex-direction: column;
  overflow-y: auto;
}

.panel-header {
  padding: 1.5rem;
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.panel-header h2 {
  margin: 0;
  font-size: 1.15rem;
  font-weight: 700;
  color: #fff;
  line-height: 1.4;
  overflow-wrap: break-word;
  min-width: 0;
}

.detail-content {
  padding: 0 1.5rem 1.5rem;
  display: flex;
  flex-direction: column;
  gap: 1.2rem;
}

.detail-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.75rem;
  border-bottom: 1px solid var(--border-light);
  padding-bottom: 0.75rem;
}

.detail-block {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  border-bottom: 1px solid var(--border-light);
  padding-bottom: 0.75rem;
}

.detail-label {
  color: var(--text-muted);
  font-size: 0.8rem;
  font-weight: 700;
  text-transform: uppercase;
  flex-shrink: 0;
}

.detail-value {
  color: var(--text-main);
  font-size: 0.95rem;
  font-weight: 600;
  text-align: right;
  overflow-wrap: anywhere;
  min-width: 0;
}

.invite-row {
  display: flex;
  align-items: center;
  gap: 0.4rem;
}

.invite-text {
  flex: 1;
  min-width: 0;
  color: var(--text-main);
  font-size: 0.8rem;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  user-select: text;
}

.icon-btn {
  background: transparent;
  border: 1px solid transparent;
  width: 26px;
  height: 24px;
  border-radius: 5px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  transition: all 0.2s;
}

.icon-btn:hover {
  border-color: var(--border-light);
  background: var(--bg-darker);
}

.detail-note {
  margin: 0;
  color: var(--text-muted);
  font-size: 0.85rem;
  line-height: 1.4;
}

.detail-note.is-error {
  color: #fb7185;
}

.pack-dirty {
  align-self: flex-start;
  background: rgba(245, 158, 11, 0.12);
  color: #f59e0b;
  border: 1px solid rgba(245, 158, 11, 0.5);
  border-radius: 99px;
  padding: 0.05rem 0.5rem;
  font-size: 0.7rem;
  font-weight: 700;
}

.action-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.pack-btn {
  width: 100%;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.6rem;
  border-radius: 8px;
  font-size: 0.9rem;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
}

.pack-btn:hover:not(:disabled) {
  background: var(--bg-hover);
}

.pack-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
</style>
