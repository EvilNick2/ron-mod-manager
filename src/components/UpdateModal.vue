<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { X, Download, RefreshCw } from "lucide-vue-next";
import { computed, onMounted, ref } from "vue";
import { store } from "../store";
import ReleaseNotesList, { type ReleaseNotesEntry } from "./ReleaseNotesList.vue";

type ReleaseEntry = {
  version: string;
  publishedAt: string;
  notes: string;
  url: string;
};

const releaseHistory = ref<ReleaseEntry[]>([]);
const historyLoading = ref(false);
const historyError = ref("");

const publishedDate = computed(() => {
  if (!store.updateInfo?.date) return "Unknown";
  const parsed = new Date(store.updateInfo.date);
  if (Number.isNaN(parsed.getTime())) return store.updateInfo.date;
  return parsed.toLocaleDateString();
});

onMounted(() => {
  void loadReleaseHistory();
});

function normalizeVersion(value: string | undefined): string {
  return (value || "").trim().replace(/^v/i, "");
}

function compareVersions(a: string, b: string): number {
  const cleanA = normalizeVersion(a).split("-")[0];
  const cleanB = normalizeVersion(b).split("-")[0];
  const partsA = cleanA.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const partsB = cleanB.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const maxLength = Math.max(partsA.length, partsB.length);

  for (let index = 0; index < maxLength; index += 1) {
    const left = partsA[index] ?? 0;
    const right = partsB[index] ?? 0;

    if (left > right) return 1;
    if (left < right) return -1;
  }

  return 0;
}

const newerReleaseHistory = computed(() => {
  const currentVersion = normalizeVersion(store.updateInfo?.currentVersion);
  if (!currentVersion) return releaseHistory.value;

  return releaseHistory.value
    .filter((release) => compareVersions(release.version, currentVersion) > 0)
    .sort((left, right) => compareVersions(right.version, left.version));
});

const newerReleaseEntries = computed<ReleaseNotesEntry[]>(() => newerReleaseHistory.value.map((release) => ({
  version: release.version,
  publishedAt: release.publishedAt ? new Date(release.publishedAt).toLocaleDateString() : publishedDate.value,
  notes: release.notes,
  url: release.url,
  latest: normalizeVersion(release.version) === normalizeVersion(store.updateInfo?.version),
})));


// The version heading repeats the card header and commit links mean nothing to players
function renderNotes(body: string): string {
  return body
    .replace(/^#{1,3} \[?\d+\.\d+\.\d+.*$/m, "")
    .replace(/ \(\[[0-9a-f]{7,40}\]\([^)]*\)\)/g, "");
}

async function loadReleaseHistory() {
  historyLoading.value = true;
  historyError.value = "";

  try {
    const response = await fetch("https://api.github.com/repos/EvilNick2/ron-mod-manager/releases?per_page=8");
    if (!response.ok) {
      throw new Error(`GitHub API returned ${response.status}`);
    }

    const payload = await response.json();
    releaseHistory.value = (Array.isArray(payload) ? payload : []).map((entry: any) => ({
      version: (entry.tag_name || entry.name || "unknown").replace(/^v/, ""),
      publishedAt: entry.published_at || "",
      notes: renderNotes(entry.body || "No release notes were provided."),
      url: entry.html_url || ""
    }));
  } catch (e) {
    console.error("Failed to load release history:", e);
    historyError.value = "Unable to load multi-version release history.";
  } finally {
    historyLoading.value = false;
  }
}

async function installUpdate() {
  if (!store.updateInfo) {
    store.updateError = "Update payload is unavailable. Please restart the app and try again.";
    return;
  }

  store.isUpdatingApp = true;
  store.updateError = "";

  try {
    await invoke("install_app_update");
  } catch (e) {
    console.error("Update installation failed:", e);
    store.updateError = "Automatic update failed. Please try again later.";
  } finally {
    store.isUpdatingApp = false;
  }
}

function closeModal() {
  if (store.isUpdatingApp) return;
  store.isUpdateModalOpen = false;
}
</script>

<template>
  <div v-if="store.isUpdateModalOpen" class="modal-overlay" @click.self="closeModal">
    <div class="update-modal">
      <button class="close-btn" @click="closeModal" :disabled="store.isUpdatingApp" aria-label="Close update modal">
        <X :size="18" color="currentColor" />
      </button>

      <h2 class="modal-title">New Update Available</h2>

      <div class="update-status-grid">
        <div class="status-card">
          <p class="status-label">Current Version</p>
          <p class="status-value">{{ store.updateInfo?.currentVersion || "Unknown" }}</p>
        </div>
        <div class="status-card">
          <p class="status-label">Update Status</p>
          <p class="status-value">Version {{ store.updateInfo?.version }} is available</p>
        </div>
      </div>

      <div class="update-details">
        <p class="details-label">Release Notes for Newer Versions</p>

        <ReleaseNotesList v-if="newerReleaseEntries.length > 0" :entries="newerReleaseEntries" />

        <p class="muted small" v-else-if="historyLoading">Loading release history…</p>
        <p class="error-text" v-else-if="historyError">{{ historyError }}</p>
        <p class="muted small" v-else>No newer release history found for your current version</p>
      </div>

      <p class="error-text" v-if="store.updateError">{{ store.updateError }}</p>

      <div class="action-row">
        <button class="secondary-btn" @click="closeModal" :disabled="store.isUpdatingApp">Later</button>
        <button class="primary-btn" @click="installUpdate" :disabled="store.isUpdatingApp">
          <RefreshCw v-if="store.isUpdatingApp" :size="16" class="spin" />
          <Download v-else :size="16" />
          {{ store.isUpdatingApp ? "Updating..." : "Update Now" }}
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
}

.update-status-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
  gap: 12px;
  margin-bottom: 12px;
}

.status-card {
  border: 1px solid var(--border, var(--border-light));
  border-radius: 6px;
  padding: 12px;
  background: var(--secondary-surface, var(--bg-card));
}

.status-label {
  margin: 0;
  color: var(--muted, var(--text-muted));
  text-transform: uppercase;
  font-size: 12px;
  letter-spacing: 0.05em;
}

.status-value {
  margin: 4px 0 0;
  font-size: 24px;
  font-weight: 700;
}

.update-details {
  display: grid;
  gap: 10px;
  padding: 12px;
  border: 1px dashed var(--border, var(--border-light));
  border-radius: 6px;
  background: var(--secondary-surface, var(--bg-card));
}

.details-label {
  margin: 0;
  text-transform: uppercase;
  font-size: 12px;
  letter-spacing: 0.06em;
  color: var(--muted, var(--text-muted));
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

.primary-btn:disabled,
.secondary-btn:disabled,
.close-btn:disabled {
  opacity: 0.7;
  cursor: not-allowed;
}

.error-text {
  color: var(--danger-text, #fca5a5);
  margin-top: 8px;
}

.muted {
  color: var(--muted, var(--text-muted));
}

.small {
  font-size: 12px;
}

.spin {
  animation: spin 1s linear infinite;
}

@keyframes spin {
  from { transform: rotate(0deg); }
  to { transform: rotate(360deg); }
}
</style>
