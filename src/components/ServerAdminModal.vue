<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, onMounted, ref } from "vue";
import { X } from "lucide-vue-next";
import { store } from "../store";
import { alertFailure, copyText, hostOf, type ServerLink } from "../packActions";
import { showConfirm } from "../dialogs";

interface AdminUser {
  id: string;
  name: string;
  role: string | null;
  packs: number;
}

const base = computed(() => store.serverAdminFor ?? "");
const users = ref<AdminUser[]>([]);
const loading = ref(true);
const busy = ref(false);

async function load() {
  loading.value = true;
  try {
    users.value = await invoke("server_users", { base: base.value }) as AdminUser[];
  } catch (e) {
    console.error("Failed to load server users:", e);
    await alertFailure("Failed to load server users: ", e);
  } finally {
    loading.value = false;
  }
}

async function copyUploaderInvite() {
  busy.value = true;
  try {
    const link = await invoke("uploader_invite_link", { base: base.value }) as ServerLink;
    await copyText(link.url);
  } catch (e) {
    console.error("Failed to copy the uploader invite link:", e);
    await alertFailure("Failed to copy the uploader invite link: ", e);
  } finally {
    busy.value = false;
  }
}

async function removeUser(user: AdminUser) {
  const question = `Remove ${user.name}? Their packs are deleted from the server.`;
  if (!(await showConfirm(question, { danger: true, confirmLabel: "Remove" }))) return;
  busy.value = true;
  try {
    await invoke("remove_server_user", { base: base.value, userId: user.id });
  } catch (e) {
    console.error("Failed to remove the user:", e);
    await alertFailure("Failed to remove the user: ", e);
  } finally {
    busy.value = false;
  }
  await load();
}

function close() {
  store.serverAdminFor = null;
}

onMounted(load);
</script>

<template>
  <div class="modal-overlay" @click.self="close">
    <div class="admin-modal">
      <button class="close-btn" @click="close">
        <X :size="20" color="#94a3b8" />
      </button>
      <h2 class="modal-title">Manage server</h2>
      <p class="admin-desc">{{ hostOf(base) }}</p>
      <button class="action-btn" @click="copyUploaderInvite" :disabled="busy">Copy uploader invite link</button>
      <h3 class="list-label">Users</h3>
      <p v-if="loading" class="list-empty">Loading...</p>
      <p v-else-if="users.length === 0" class="list-empty">No users yet.</p>
      <div v-else class="user-list">
        <div v-for="user in users" :key="user.id" class="user-row">
          <span class="user-name">{{ user.name }}</span>
          <span class="user-meta">{{ user.role ?? "viewer" }}</span>
          <span class="user-meta">{{ user.packs === 1 ? "1 pack" : `${user.packs} packs` }}</span>
          <button
            v-if="user.role !== 'owner'"
            class="remove-btn"
            @click="removeUser(user)"
            :disabled="busy"
          >Remove</button>
        </div>
      </div>
      <div class="modal-actions">
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

.admin-modal {
  position: relative;
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  border-radius: 12px;
  width: 560px;
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

.admin-desc {
  color: var(--text-muted);
  font-size: 0.95rem;
  margin: 0 0 1.5rem;
  text-align: center;
  word-break: break-all;
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
  align-self: flex-start;
}

.action-btn:hover:not(:disabled) {
  background: var(--accent-hover);
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

.user-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

.user-row {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  border-radius: 8px;
  padding: 0.6rem 0.9rem;
}

.user-name {
  flex: 1;
  color: var(--text-main);
  font-size: 0.9rem;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.user-meta {
  color: var(--text-muted);
  font-size: 0.8rem;
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
  justify-content: flex-end;
  gap: 1rem;
  margin-top: 1.5rem;
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

.action-btn:disabled, .remove-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
