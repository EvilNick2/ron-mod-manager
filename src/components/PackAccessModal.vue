<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, onMounted, ref, watch } from "vue";
import { X } from "lucide-vue-next";
import { store } from "../store";
import {
  alertFailure, beginOp, copyInviteLink, copyText, endOp, hostOf, refreshMods, type ServerLink
} from "../packActions";
import { showAlert, showConfirm } from "../dialogs";

interface Viewer {
  id: string;
  name: string;
}

const pack = computed(() => store.packs.find((p) => p.pack_id === store.packAccessFor) ?? null);
const base = computed(() => pack.value?.published?.server ?? "");
const viewers = ref<Viewer[]>([]);
const loading = ref(true);
const busy = ref(false);

function close() {
  store.packAccessFor = null;
}

// The pack can vanish underneath, for example when it is deleted from the sidebar
watch(pack, (p) => {
  if (!p) close();
});

async function load() {
  if (!pack.value || !base.value) {
    close();
    return;
  }
  loading.value = true;
  try {
    viewers.value = await invoke("pack_viewers", { base: base.value, packId: pack.value.pack_id }) as Viewer[];
  } catch (e) {
    console.error("Failed to load viewers:", e);
    await alertFailure("Failed to load viewers: ", e);
  } finally {
    loading.value = false;
  }
}

async function removeViewer(viewer: Viewer) {
  const p = pack.value;
  if (!p) return;
  if (!(await showConfirm(`Remove ${viewer.name} from ${p.name}?`, { danger: true, confirmLabel: "Remove" }))) return;
  busy.value = true;
  try {
    const removed = await invoke("remove_pack_viewer", {
      base: base.value, packId: p.pack_id, userId: viewer.id,
    }) as { invite_reset: boolean };
    if (removed.invite_reset) {
      await showAlert("The invite link was reset. Copy the new one to share the pack again.");
    }
  } catch (e) {
    console.error("Failed to remove the viewer:", e);
    await alertFailure("Failed to remove the viewer: ", e);
  } finally {
    busy.value = false;
  }
  await load();
}

async function copyLink() {
  if (pack.value) await copyInviteLink(pack.value);
}

async function resetInvite() {
  const p = pack.value;
  if (!p) return;
  const question = "Reset the invite link? The current link stops working for anyone you shared it with.";
  if (!(await showConfirm(question, { danger: true, confirmLabel: "Reset" }))) return;
  busy.value = true;
  try {
    const link = await invoke("reset_pack_invite", { base: base.value, packId: p.pack_id }) as ServerLink;
    await copyText(link.url);
    await showAlert("The invite link is reset and the new one is copied. The old link no longer works.");
  } catch (e) {
    console.error("Failed to reset the invite link:", e);
    await alertFailure("Failed to reset the invite link: ", e);
  } finally {
    busy.value = false;
  }
}

async function deleteFromServer() {
  const p = pack.value;
  if (!p || store.inFlight[p.pack_id]) return;
  const question = `Delete ${p.name} from ${hostOf(base.value)}? Players can no longer install or update it. Your local copy stays.`;
  if (!(await showConfirm(question, { danger: true, confirmLabel: "Delete" }))) return;
  if (store.inFlight[p.pack_id]) return;
  busy.value = true;
  beginOp(p.pack_id, p.name);
  try {
    await invoke("delete_server_pack", { packId: p.pack_id });
  } catch (e) {
    console.error("Failed to delete the pack from the server:", e);
    endOp(p.pack_id);
    busy.value = false;
    await alertFailure("Failed to delete the pack from the server: ", e);
    return;
  }
  endOp(p.pack_id);
  busy.value = false;
  close();
  await refreshMods().catch((e) => console.error("Failed to refresh mods:", e));
}

onMounted(load);
</script>

<template>
  <div v-if="pack" class="modal-overlay" @click.self="close">
    <div class="access-modal">
      <button class="close-btn" @click="close">
        <X :size="20" color="#94a3b8" />
      </button>
      <h2 class="modal-title">Manage access</h2>
      <p class="access-desc">{{ pack.name }} on {{ hostOf(base) }}</p>
      <div class="access-actions">
        <button class="action-btn" @click="copyLink" :disabled="busy">Copy invite link</button>
        <button class="secondary-action-btn" @click="resetInvite" :disabled="busy">Reset invite link</button>
      </div>
      <h3 class="list-label">Players with access</h3>
      <p v-if="loading" class="list-empty">Loading...</p>
      <p v-else-if="viewers.length === 0" class="list-empty">Nobody has joined yet.</p>
      <div v-else class="viewer-list">
        <div v-for="viewer in viewers" :key="viewer.id" class="viewer-row">
          <span class="viewer-name">{{ viewer.name }}</span>
          <button class="remove-btn" @click="removeViewer(viewer)" :disabled="busy">Remove</button>
        </div>
      </div>
      <div class="modal-actions">
        <button class="danger-btn" @click="deleteFromServer" :disabled="busy">Delete from server</button>
        <button class="secondary-btn" @click="close">Close</button>
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
  z-index: 1500;
  display: flex;
  justify-content: center;
  align-items: center;
}

.access-modal {
  position: relative;
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  border-radius: 12px;
  width: 520px;
  max-width: 90vw;
  max-height: 84vh;
  overflow: auto;
  padding: 2.5rem;
  box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 1);
  display: flex;
  flex-direction: column;
}

.close-btn {
  position: absolute;
  top: 1rem;
  right: 1rem;
  background: transparent;
  border: none;
  cursor: pointer;
  padding: 0.25rem;
}

.modal-title {
  margin: 0 0 0.5rem 0;
  color: #fff;
  font-size: 1.5rem;
  font-weight: 700;
  text-align: center;
}

.access-desc {
  color: var(--text-muted);
  font-size: 0.95rem;
  margin: 0 0 1.5rem;
  text-align: center;
  overflow-wrap: anywhere;
}

.access-actions {
  display: flex;
  gap: 0.75rem;
}

.action-btn {
  background: var(--accent-primary);
  color: var(--text-main);
  border: none;
  padding: 0.75rem 1.25rem;
  border-radius: 8px;
  font-weight: 700;
  cursor: pointer;
  transition: all 0.2s;
}

.action-btn:hover:not(:disabled) {
  background: var(--accent-hover);
}

.secondary-action-btn {
  background: var(--bg-hover);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.75rem 1.25rem;
  border-radius: 8px;
  cursor: pointer;
  font-weight: 600;
  transition: all 0.2s;
}

.secondary-action-btn:hover:not(:disabled) {
  background: var(--bg-card);
  border-color: var(--text-muted);
}

.list-label {
  color: var(--text-main);
  font-size: 1rem;
  margin: 1.5rem 0 0.75rem;
}

.list-empty {
  color: var(--text-muted);
  font-size: 0.9rem;
  margin: 0;
}

.viewer-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.viewer-row {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  border-radius: 8px;
  padding: 0.6rem 0.9rem;
}

.viewer-name {
  flex: 1;
  color: var(--text-main);
  font-size: 0.9rem;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.remove-btn {
  background: rgba(239, 68, 68, 0.12);
  border: 1px solid rgba(239, 68, 68, 0.5);
  color: #ef4444;
  border-radius: 6px;
  font-size: 0.8rem;
  font-weight: 700;
  padding: 0.2rem 0.75rem;
  cursor: pointer;
}

.modal-actions {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  margin-top: 1.5rem;
}

.danger-btn {
  padding: 0.75rem 1.25rem;
  border-radius: 8px;
  font-weight: 700;
  font-size: 0.95rem;
  cursor: pointer;
  transition: all 0.2s;
  background: rgba(239, 68, 68, 0.12);
  border: 1px solid rgba(239, 68, 68, 0.5);
  color: #ef4444;
}

.secondary-btn {
  padding: 0.75rem 1.75rem;
  border-radius: 8px;
  font-weight: 700;
  font-size: 0.95rem;
  cursor: pointer;
  transition: all 0.2s;
  border: none;
  background: transparent;
  color: var(--text-muted);
}

.secondary-btn:hover {
  color: #fff;
  background: var(--bg-card);
}

.action-btn:disabled, .secondary-action-btn:disabled, .remove-btn:disabled, .danger-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
