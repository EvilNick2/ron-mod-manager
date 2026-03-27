<script setup lang="ts">
import { store } from "../store";
import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";
import { Archive, FileArchive, FolderOpen, Puzzle, Trash2, Plus } from 'lucide-vue-next';

const nexusInput = ref("");
const isInstalling = ref(false);
const errorMsg = ref("");
const postInstallAddons = ref<string[]>([]);

watch(() => store.awaitingDropForId, (newId) => {
  if (newId) {
    nexusInput.value = newId;
  }
}, { immediate: true });

function extractModId(input: string): string | null {
  let str = input.trim();
  if (/^\d+$/.test(str)) {
    return str;
  }
  const match = str.match(/(?:mods\/|id=)(\d+)/i);
  if (match && match[1]) {
    return match[1];
  }
  return null;
}

async function startInstall() {
  errorMsg.value = "";
  if (!store.installingModPath) return;

  const modId = extractModId(nexusInput.value);
  if (!modId) {
    errorMsg.value = "Please enter a valid Nexus Mod ID or URL slice. Example: 3187";
    return;
  }

  isInstalling.value = true;
  try {
    console.log("[InstallModal] Starting install for modId:", modId, "path:", store.installingModPath);
    await invoke("install_mod_archive", { 
      archivePath: store.installingModPath, 
      modId: modId
    });
    
    console.log("[InstallModal] Install succeeded, scanning mods...");
    store.mods = await invoke("scan_local_mods");
    
    console.log("[InstallModal] Setting postInstallModId to:", modId);
    store.postInstallModId = modId;
    store.installingModPath = null;
    store.awaitingDropForId = null;
    nexusInput.value = "";
  } catch(e: any) {
    console.error("[InstallModal] Install failed:", e);
    errorMsg.value = typeof e === 'string' ? e : JSON.stringify(e);
  } finally {
    isInstalling.value = false;
  }
}

function finishInstall() {
  store.postInstallModId = null;
  postInstallAddons.value = [];
  errorMsg.value = "";
}

async function pickPostInstallAddon() {
  if (!store.postInstallModId) return;
  try {
    const file: string | null = await invoke("pick_mod_archive");
    if (file) {
      const added: string[] = await invoke("install_addon", { modId: store.postInstallModId, archivePath: file });
      postInstallAddons.value = [...postInstallAddons.value, ...added];
      store.mods = await invoke("scan_local_mods");
    }
  } catch (e: any) {
    errorMsg.value = "Failed to add addon: " + e;
  }
}

async function removePostInstallAddon(filename: string) {
  if (!store.postInstallModId) return;
  try {
    await invoke("remove_addon", { modId: store.postInstallModId, filename });
    postInstallAddons.value = postInstallAddons.value.filter(f => f !== filename);
    store.mods = await invoke("scan_local_mods");
  } catch (e: any) {
    errorMsg.value = "Failed to remove addon: " + e;
  }
}

function cancelInstall() {
  store.installingModPath = null;
  store.awaitingDropForId = null;
  store.postInstallModId = null;
  postInstallAddons.value = [];
  nexusInput.value = "";
  errorMsg.value = "";
}

async function browseForArchive() {
  try {
    const path: string | null = await invoke("pick_mod_archive");
    if (path) {
      store.installingModPath = path;
    }
  } catch (e) {
    console.error("Failed to pick archive:", e);
  }
}
</script>

<template>
  <div v-if="store.installingModPath || store.awaitingDropForId || store.postInstallModId" class="modal-overlay">
    
    <div v-if="store.postInstallModId" class="install-modal">
      <h2 class="modal-title">Add Optional Files?</h2>
      
      <p class="install-desc">
        Mod installed successfully! You can now add optional addon files (.pak or .zip).
      </p>
      
      <div class="addons-post-list" v-if="postInstallAddons.length">
        <div v-for="fname in postInstallAddons" :key="fname" class="addon-post-row">
          <Puzzle :size="14" color="#f59e0b" />
          <span class="addon-post-name">{{ fname.replace('.pak', '') }}</span>
          <button class="addon-post-delete" @click="removePostInstallAddon(fname)">
            <Trash2 :size="14" color="#ef4444" />
          </button>
        </div>
      </div>
      
      <button class="add-addon-modal-btn" @click="pickPostInstallAddon">
        <Plus :size="16" color="#22c55e" /> Browse for Addon
      </button>
      
      <div v-if="errorMsg" class="error-msg">{{ errorMsg }}</div>
      
      <div class="modal-actions">
        <button class="primary-btn" @click="finishInstall">Done</button>
      </div>
    </div>
    
    <div v-else class="install-modal" :class="{ 'waiting-pulse': store.awaitingDropForId && !store.installingModPath }">
      <h2 class="modal-title">Install Mod Archive</h2>
      
      <div v-if="store.installingModPath" class="archive-info">
        <Archive :size="18" color="#f59e0b" />
        <code>{{ store.installingModPath }}</code>
      </div>
      
      <div v-else class="archive-info drop-pulse" style="border-style: dashed; justify-content: center; color: var(--text-muted); flex-direction: column; align-items: center; gap: 0.75rem;">
        <FileArchive :size="32" color="#3b82f6" class="icon" />
        <span>Drop .zip file here or browse</span>
        <button class="browse-btn" @click="browseForArchive">
          <FolderOpen :size="14" color="#fbbf24" /> Browse for archive
        </button>
      </div>

      <p class="install-desc" v-if="store.installingModPath">
        To properly track this mod inside the manager with thumbnails and descriptions, we need its <strong>Nexus Mods ID</strong> or website link. 
      </p>
      
      <div class="input-group">
        <label>Nexus Mod URL or ID</label>
        <input 
          v-model="nexusInput" 
          type="text" 
          placeholder="e.g. 3187 or https://www.nexusmods.com/readyornot/mods/3187" 
          class="dark-input" 
          :disabled="isInstalling"
        />
      </div>
      
      <div v-if="errorMsg" class="error-msg">{{ errorMsg }}</div>

      <div class="modal-actions">
        <button class="secondary-btn" @click="cancelInstall" :disabled="isInstalling">Cancel</button>
        <button class="primary-btn" @click="startInstall" :disabled="isInstalling || !store.installingModPath">
          {{ isInstalling ? 'Extracting...' : 'Install Mod' }}
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
  z-index: 2000;
  display: flex;
  justify-content: center;
  align-items: center;
}

.install-modal {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  border-radius: 12px;
  width: 500px;
  padding: 2.5rem;
  box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 1);
  display: flex;
  flex-direction: column;
  transition: all 0.3s ease;
}

.waiting-pulse {
  border-color: rgba(59, 130, 246, 0.5);
  box-shadow: 0 0 40px rgba(59, 130, 246, 0.2);
}

.modal-title {
  margin: 0 0 1.5rem 0;
  color: #fff;
  font-size: 1.5rem;
  font-weight: 700;
  text-align: center;
}

.archive-info {
  background: var(--bg-card);
  padding: 1rem;
  border-radius: 8px;
  border: 1px solid var(--border-dark);
  display: flex;
  align-items: center;
  gap: 0.75rem;
  margin-bottom: 1.5rem;
  word-break: break-all;
  font-family: monospace;
  color: var(--accent-primary);
  font-size: 0.85rem;
}

.install-desc {
  color: var(--text-muted);
  font-size: 0.95rem;
  line-height: 1.5;
  margin-bottom: 2rem;
  text-align: center;
}

.input-group {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin-bottom: 1.5rem;
}

.input-group label {
  color: var(--text-main);
  font-size: 0.9rem;
  font-weight: 600;
}

.dark-input {
  background: rgba(0,0,0,0.3);
  border: 1px solid var(--border-dark);
  color: #fff;
  padding: 0.85rem 1rem;
  border-radius: 6px;
  font-size: 1rem;
  outline: none;
  transition: all 0.2s;
}

.dark-input:focus {
  border-color: var(--accent-primary);
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

.secondary-btn:hover {
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

.primary-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.error-msg {
  color: #ef4444;
  font-size: 0.85rem;
  margin-bottom: 1rem;
  text-align: center;
}

.browse-btn {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.5rem 1rem;
  border-radius: 6px;
  font-size: 0.8rem;
  font-weight: 600;
  cursor: pointer;
  transition: border-color 0.2s, background 0.2s;
}

.browse-btn:hover {
  border-color: var(--accent-primary);
  background: var(--bg-hover);
}

.addons-post-list {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  margin-bottom: 1rem;
}

.addon-post-row {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  background: var(--bg-card);
  padding: 0.6rem 0.75rem;
  border-radius: 6px;
  border: 1px solid var(--border-dark);
}

.addon-post-name {
  flex: 1;
  font-size: 0.85rem;
  color: var(--text-main);
  font-weight: 500;
}

.addon-post-delete {
  background: none;
  border: none;
  cursor: pointer;
  opacity: 0.5;
  transition: opacity 0.15s;
  display: flex;
  align-items: center;
}

.addon-post-delete:hover {
  opacity: 1;
}

.add-addon-modal-btn {
  width: 100%;
  background: transparent;
  border: 1px dashed var(--border-light);
  color: var(--text-muted);
  padding: 0.75rem;
  border-radius: 8px;
  font-size: 0.9rem;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  margin-bottom: 1rem;
  transition: all 0.2s;
}

.add-addon-modal-btn:hover {
  border-color: #22c55e;
  background: rgba(34, 197, 94, 0.05);
  color: var(--text-main);
}
</style>
