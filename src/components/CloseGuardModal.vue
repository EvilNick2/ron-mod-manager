<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, ref } from "vue";
import { store, type PackRef } from "../store";
import { loadPacks, publishPack, progressLine, progressPercent } from "../packActions";
import { showAlert } from "../dialogs";

const busy = ref(false);

const uploading = computed(() => store.closeBlocked?.uploading ?? []);
const downloading = computed(() => store.closeBlocked?.downloading ?? []);
const transferring = computed(() => uploading.value.length > 0 || downloading.value.length > 0);

const progress = computed(() => Object.values(store.packProgress).map((p) => ({
  key: p.pack_id,
  name: store.packs.find((pack) => pack.pack_id === p.pack_id)?.name ?? p.pack_id,
  line: progressLine(p),
  percent: progressPercent(p),
})));

function joinNames(packs: PackRef[]): string {
  const names = packs.map((p) => p.name);
  return names.length > 1
    ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`
    : names.join("");
}

function stillDoing(packs: PackRef[], what: string): string {
  return `${joinNames(packs)} ${packs.length > 1 ? "are" : "is"} still ${what}`;
}

const message = computed(() => {
  if (transferring.value) {
    const parts = [];
    if (uploading.value.length > 0) parts.push(stillDoing(uploading.value, "uploading"));
    if (downloading.value.length > 0) parts.push(stillDoing(downloading.value, "downloading"));
    return `${parts.join(" and ")}. Close anyway, or cancel?`;
  }
  const dirty = store.closeBlocked?.dirty ?? [];
  const verb = dirty.length > 1 ? "have" : "has";
  return `${joinNames(dirty)} ${verb} changes that are not uploaded. Upload now, close anyway, or cancel?`;
});

const title = computed(() => {
  if (uploading.value.length > 0 && downloading.value.length > 0) return "Transfers In Progress";
  if (uploading.value.length > 0) return "Upload In Progress";
  if (downloading.value.length > 0) return "Download In Progress";
  return "Changes Not Uploaded";
});

async function forceClose() {
  try {
    await invoke("force_close");
  } catch (e) {
    console.error("Failed to close:", e);
    await showAlert("Failed to close: " + e);
  }
}

async function uploadNow() {
  busy.value = true;
  try {
    const dirty = await invoke("list_dirty_packs") as PackRef[];
    if (dirty.length > 0) store.closeBlocked = { dirty, uploading: [], downloading: [] };
    await loadPacks();
    for (const entry of dirty) {
      const pack = store.packs.find((p) => p.pack_id === entry.pack_id);
      if (!pack) {
        await showAlert(`Could not find ${entry.name} to upload it.`);
        return;
      }
      if (!(await publishPack(pack))) return;
    }
    await forceClose();
  } catch (e) {
    console.error("Failed to upload before closing:", e);
    await showAlert("Failed to upload: " + e);
  } finally {
    busy.value = false;
  }
}

function cancel() {
  store.closeBlocked = null;
}
</script>

<template>
  <div v-if="store.closeBlocked" class="modal-overlay">
    <div class="guard-modal">
      <h2 class="modal-title">{{ title }}</h2>
      <p class="guard-desc">{{ message }}</p>
      <div v-for="p in progress" :key="p.key" class="guard-progress">
        <p class="guard-progress-line">{{ p.name }}: {{ p.line }}</p>
        <div class="guard-progress-track"><div class="guard-progress-fill" :style="{ width: p.percent + '%' }"></div></div>
      </div>
      <div class="modal-actions">
        <button class="secondary-btn" @click="cancel" :disabled="busy">Cancel</button>
        <button class="secondary-btn" @click="forceClose" :disabled="busy">Close anyway</button>
        <button v-if="!transferring" class="primary-btn" @click="uploadNow" :disabled="busy">
          {{ busy ? "Uploading..." : "Upload now" }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.modal-overlay {
  position: fixed;
  top: 0; left: 0; right: 0; bottom: 0;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(8px);
  z-index: 3000;
  display: flex;
  justify-content: center;
  align-items: center;
}

.guard-modal {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  border-radius: 12px;
  width: 500px;
  max-width: 90vw;
  padding: 2.5rem;
  box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 1);
  display: flex;
  flex-direction: column;
}

.modal-title {
  margin: 0 0 1.5rem 0;
  color: #fff;
  font-size: 1.5rem;
  font-weight: 700;
  text-align: center;
}

.guard-desc {
  color: var(--text-muted);
  font-size: 0.95rem;
  line-height: 1.5;
  margin: 0 0 1.5rem;
  text-align: center;
}

.guard-progress {
  margin: 0 0 1rem;
}

.guard-progress-line {
  color: var(--text-muted);
  font-size: 0.85rem;
  margin: 0 0 0.4rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.guard-progress-track {
  height: 6px;
  border-radius: 3px;
  background: var(--bg-card);
  overflow: hidden;
}

.guard-progress-fill {
  height: 100%;
  background: var(--accent-primary);
  transition: width 0.2s;
}

.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 1rem;
  margin-top: 1rem;
}

.secondary-btn, .primary-btn {
  padding: 0.75rem 1.75rem;
  border-radius: 8px;
  font-weight: 700;
  font-size: 0.95rem;
  cursor: pointer;
  transition: all 0.2s;
  border: none;
}

.secondary-btn {
  background: transparent;
  color: var(--text-muted);
}

.secondary-btn:hover:not(:disabled) {
  color: #fff;
  background: var(--bg-card);
}

.primary-btn {
  background: var(--accent-primary);
  color: var(--text-main);
  box-shadow: 0 4px 6px -1px rgba(230,29,54,0.3);
}

.primary-btn:hover:not(:disabled) {
  background: var(--accent-hover);
  transform: translateY(-2px);
}

.primary-btn:disabled, .secondary-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
