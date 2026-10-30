<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { X, Download } from "lucide-vue-next";
import { computed, ref, watch } from "vue";
import { store } from "../store";
import { closePackChangelog } from "../packActions";
import ReleaseNotesList, { type ReleaseNotesEntry } from "./ReleaseNotesList.vue";

interface VersionEntry {
  version: string;
  published_at: number;
  notes: string;
  publisher?: string | null;
}

interface PackChangelog {
  entries: VersionEntry[];
  saved: boolean;
}

const request = computed(() => store.packChangelog);
const pack = computed(() => store.packs.find((p) => p.pack_id === request.value?.packId) ?? null);
const updating = computed(() => request.value?.mode === "update");
const entries = ref<VersionEntry[]>([]);
const saved = ref(false);
const loading = ref(true);
const loadError = ref("");

const title = computed(() => {
  const name = pack.value?.name ?? request.value?.packId ?? "";
  return updating.value ? `Update ${name}` : `${name} changelog`;
});

// Entries come newest first. An update shows the ones above the installed version, or all when it is not listed.
const shown = computed<ReleaseNotesEntry[]>(() => {
  let list = entries.value;
  if (updating.value) {
    const installed = list.findIndex((e) => e.version === pack.value?.version);
    if (installed >= 0) list = list.slice(0, installed);
  }
  return list.map((e, i) => ({
    version: e.version.replace(/^[vV]/, ""),
    publishedAt: new Date(e.published_at * 1000).toLocaleDateString() + (e.publisher ? ` by ${e.publisher}` : ""),
    notes: e.notes,
    latest: i === 0,
  }));
});

// The backend falls back to the saved copy only for a server pack it could not reach
const showSavedNote = computed(() => saved.value && pack.value?.source?.kind === "server");

watch(pack, (p) => {
  if (!p) closePackChangelog(false);
}, { immediate: true });

let loadSeq = 0;

async function load() {
  const packId = request.value?.packId;
  if (!packId) return;
  const seq = ++loadSeq;
  loading.value = true;
  loadError.value = "";
  entries.value = [];
  saved.value = false;
  try {
    const changelog = await invoke("pack_changelog", { packId }) as PackChangelog;
    if (seq !== loadSeq) return;
    entries.value = changelog.entries;
    saved.value = changelog.saved;
  } catch (e) {
    console.error("Failed to load the changelog:", e);
    if (seq === loadSeq) loadError.value = `Failed to load the changelog: ${e}`;
  } finally {
    if (seq === loadSeq) loading.value = false;
  }
}

function close() {
  closePackChangelog(false);
}

function update() {
  if (pack.value && store.inFlight[pack.value.pack_id]) return;
  closePackChangelog(true);
}

watch(request, (r) => {
  if (r) load();
}, { immediate: true });
</script>

<template>
  <div v-if="request" class="modal-overlay" @click.self="close">
    <div class="update-modal">
      <button class="close-btn" @click="close" aria-label="Close changelog">
        <X :size="18" color="currentColor" />
      </button>

      <h2 class="modal-title">{{ title }}</h2>

      <p v-if="updating && request.summary" class="summary-line">{{ request.summary }}</p>

      <div class="update-details">
        <p v-if="showSavedNote" class="muted small">Showing the saved changelog</p>
        <ReleaseNotesList v-if="shown.length > 0" :entries="shown" />
        <p class="muted small" v-else-if="loading">Loading changelog...</p>
        <p class="error-text" v-else-if="loadError">{{ loadError }}</p>
        <p class="muted small" v-else>No changelog entries</p>
      </div>

      <div v-if="updating" class="action-row">
        <button class="secondary-btn" @click="close">Later</button>
        <button class="primary-btn" @click="update" :disabled="!!(pack && store.inFlight[pack.pack_id])">
          <Download :size="16" />
          Update
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.modal-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  backdrop-filter: blur(8px);
  z-index: 1100;
  display: flex;
  justify-content: center;
  align-items: center;
}

.update-modal {
  position: relative;
  width: min(980px, 94vw);
  max-height: 84vh;
  overflow: auto;
  padding: 1.1rem;
  border: 1px solid var(--border, var(--border-light));
  border-radius: 8px;
  background: var(--surface, var(--bg-dark));
}

.close-btn {
  position: absolute;
  top: 0.65rem;
  right: 0.65rem;
  width: 30px;
  height: 30px;
  border: 1px solid var(--border, var(--border-light));
  border-radius: 6px;
  background: var(--button-bg, var(--bg-card));
  color: var(--muted, var(--text-muted));
  cursor: pointer;
}

.close-btn:hover {
  background: var(--button-hover, var(--bg-hover));
}

.modal-title {
  margin: 0 0 0.9rem;
  color: var(--accent-primary);
  text-align: center;
  font-size: 2rem;
  font-weight: 800;
  overflow-wrap: anywhere;
}

.summary-line {
  margin: 0 0 12px;
  color: var(--text-main);
  font-size: 14px;
  line-height: 1.5;
}

.update-details {
  display: grid;
  gap: 10px;
  padding: 12px;
  border: 1px dashed var(--border, var(--border-light));
  border-radius: 6px;
  background: var(--secondary-surface, var(--bg-card));
}

.action-row {
  margin-top: 12px;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.primary-btn,
.secondary-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  border-radius: 6px;
  font-weight: 700;
  cursor: pointer;
}

.primary-btn {
  border: none;
  background: var(--accent-primary, #e61d36);
  color: #fff;
}

.secondary-btn {
  border: 1px solid var(--border, var(--border-light));
  background: var(--button-bg, var(--bg-card));
  color: var(--button-text, var(--text-main));
}

.primary-btn:disabled {
  opacity: 0.7;
  cursor: not-allowed;
}

.error-text {
  color: var(--danger-text, #fca5a5);
  margin: 0;
}

.muted {
  color: var(--muted, var(--text-muted));
}

.small {
  font-size: 12px;
  margin: 0;
}
</style>
