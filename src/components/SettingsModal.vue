<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { store } from "../store";
import { onMounted, ref } from "vue";

const activeTab = ref("Global");
const pendingGamePath = ref("");

onMounted(async () => {
  try {
    store.defaultModStoragePath = await invoke("get_default_storage_path");
    
    const config: any = await invoke("load_config");
    if (config.mod_storage_path) {
      store.modStoragePathStore = config.mod_storage_path;
    }
    
    if (config.enable_blur !== undefined && config.enable_blur !== null) {
      store.isBlurEnabled = config.enable_blur;
    }
    
    const key: string | null = await invoke("get_api_key");
    if (key) {
      store.apiKeyStore = key;
    }

    pendingGamePath.value = store.gamePathStore;
  } catch (error) {
    console.error(error);
  }
});

async function manualPickGame() {
  try {
    const folder: string | null = await invoke("pick_game_folder");
    if (folder) {
      pendingGamePath.value = folder;
    }
  } catch (e) {
    console.error(e);
  }
}

async function autoDetectGame() {
  try {
    const detected: string | null = await invoke("auto_detect_game_path");
    if (detected) {
      pendingGamePath.value = detected;
    } else {
      alert("Could not automatically locate Ready or Not across any known Steam Libraries.");
    }
  } catch (e) {
    console.error("Auto detect failed:", e);
    alert("System restriction blocked auto-detection. Try picking manually.");
  }
}

async function pickStorageFolder() {
  try {
    const folder: string | null = await invoke("pick_storage_folder");
    if (folder) {
      store.modStoragePathStore = folder;
      const config: any = await invoke("load_config");
      config.mod_storage_path = folder;
      await invoke("save_config", { config });
      
      store.mods = await invoke("scan_local_mods");
    }
  } catch (e) {
    console.error("Storage Folder picker failed", e);
  }
}

async function saveGamePath() {
  try {
    if (pendingGamePath.value) {
      store.gamePathStore = pendingGamePath.value;
      const config: any = await invoke("load_config");
      config.game_path = pendingGamePath.value;
      await invoke("save_config", { config });
    }
  } catch (e) {
    console.error("Failed to save game path", e);
  }
}

async function toggleBlur(state: boolean) {
  store.isBlurEnabled = state;
  try {
    const config: any = await invoke("load_config");
    config.enable_blur = state;
    await invoke("save_config", { config });
  } catch(e) {
    console.error("Failed to save blur config", e);
  }
}
</script>

<template>
  <div v-if="store.isSettingsOpen" class="modal-overlay" @click.self="store.isSettingsOpen = false">
    <div class="settings-modal">
      <button class="close-btn" @click="store.isSettingsOpen = false">✕</button>
      <h2 class="modal-title">Settings</h2>
      
      <div class="settings-tabs">
        <button class="tab-btn" :class="{ active: activeTab === 'Global' }" @click="activeTab = 'Global'">
          <span class="icon">🌐</span> Global
        </button>
        <button class="tab-btn" :class="{ active: activeTab === 'Ready or Not' }" @click="activeTab = 'Ready or Not'">
          <span class="icon">🔫</span> Ready or Not
        </button>
      </div>

      <div class="settings-content">
        <template v-if="activeTab === 'Global'">
          <div class="settings-section col-span-full">
            <h3>Local Mod Storage Location ℹ️</h3>
            <p class="settings-desc">Select the master folder where mods are saved. A `ReadyOrNot` child folder is generated here automatically.</p>
            <div class="path-picker-row">
              <button class="action-btn" @click="pickStorageFolder">Change</button>
              <input type="text" readonly :value="store.modStoragePathStore || store.defaultModStoragePath" placeholder="Default System Storage..." class="path-input" />
            </div>
          </div>
          
          <div class="settings-section">
            <h3>Nexus Mods API Configuration ℹ️</h3>
            <p class="settings-desc" style="margin-bottom: 1rem;">Your API Key is actively stored and securely managed. Re-login from sidebar to update.</p>
            <div class="api-key-box">
              <code v-if="store.apiKeyStore">{{ store.apiKeyStore.substring(0, 15) }}...*****************</code>
              <code v-else>Authentication is completely missing.</code>
            </div>
          </div>

          <div class="settings-section">
            <h3>Modal Background Appearance ℹ️</h3>
            <p class="settings-desc" style="margin-bottom: 1rem;">Toggle the cinematic gaussian blur effect when overlay menus are open.</p>
            <div class="settings-tabs" style="margin-bottom: 0;">
              <button class="tab-btn" :class="{ active: store.isBlurEnabled }" @click="toggleBlur(true)">Blur Enabled</button>
              <button class="tab-btn" :class="{ active: !store.isBlurEnabled }" @click="toggleBlur(false)">Blur Disabled</button>
            </div>
          </div>
        </template>

        <template v-if="activeTab === 'Ready or Not'">
          <div class="settings-section col-span-full">
            <h3>Game Installation Path ℹ️</h3>
            <p class="settings-desc">Select the folder where Ready Or Not is installed (contains Paks/mods).</p>
            <div class="path-picker-row" style="margin-bottom: 1rem;">
              <input type="text" v-model="pendingGamePath" placeholder="C:\Program Files (x86)\... or /home/..." class="path-input" />
              <button class="action-btn" @click="autoDetectGame" title="Scan default Steam directories">Auto-Detect</button>
              <button class="action-btn" @click="manualPickGame">Browse...</button>
            </div>
            <button class="action-btn save-btn" @click="saveGamePath" :disabled="pendingGamePath === store.gamePathStore">Save Game Path</button>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.4);
  backdrop-filter: blur(8px);
  z-index: 1000;
  display: flex;
  justify-content: center;
  align-items: center;
}

.settings-modal {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  border-radius: 12px;
  width: 750px;
  max-width: 90vw;
  max-height: 85vh;
  padding: 2.5rem;
  position: relative;
  box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 1);
  display: flex;
  flex-direction: column;
}

.close-btn {
  position: absolute;
  top: 1rem;
  right: 1.5rem;
  background: transparent;
  border: none;
  color: var(--text-muted);
  font-size: 1.25rem;
  font-weight: 900;
  cursor: pointer;
  transition: color 0.2s;
}

.close-btn:hover {
  color: var(--accent-primary);
}

.modal-title {
  text-align: center;
  color: var(--accent-primary);
  margin-top: 0;
  margin-bottom: 2rem;
  font-size: 2rem;
  font-weight: 800;
}

.settings-tabs {
  display: flex;
  gap: 1rem;
  margin-bottom: 2rem;
}

.tab-btn {
  flex: 1;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  color: var(--text-muted);
  padding: 0.85rem;
  border-radius: 8px;
  font-size: 1rem;
  font-weight: 700;
  cursor: pointer;
  transition: all 0.2s;
}

.tab-btn:hover {
  border-color: var(--text-muted);
}

.tab-btn.active {
  background: transparent;
  color: var(--text-main);
  border-color: var(--accent-primary);
}

.settings-content {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 2.5rem;
  overflow-y: auto;
}

.col-span-full {
  grid-column: 1 / -1;
  background: var(--bg-card);
  padding: 1.5rem;
  border-radius: 12px;
  border: 1px solid var(--border-light);
}

.settings-section {
  display: flex;
  flex-direction: column;
}

.settings-section h3 {
  color: var(--text-main);
  font-size: 1.1rem;
  margin: 0 0 0.5rem 0;
}

.action-btn {
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.75rem 1.5rem;
  border-radius: 8px;
  cursor: pointer;
  font-weight: 600;
  transition: all 0.2s;
}

.action-btn:hover {
  background: var(--bg-hover);
  border-color: var(--accent-primary);
}

.path-picker-row {
  display: flex;
  gap: 1rem;
  align-items: center;
}

.path-input {
  flex: 1;
  background: rgba(0,0,0,0.3);
  color: var(--text-muted);
  border: 1px solid var(--border-dark);
  border-radius: 4px;
  padding: 0.75rem 1rem;
  font-family: monospace;
  font-size: 0.9rem;
  outline: none;
}

.settings-desc {
  color: var(--text-muted);
  font-size: 0.9rem;
  line-height: 1.4;
  margin-top: 0;
  margin-bottom: 1.5rem;
}

.api-key-box {
  background: rgba(0,0,0,0.3);
  padding: 1rem;
  border: 1px solid var(--border-dark);
  border-radius: 8px;
  font-family: monospace;
  font-size: 0.85rem;
  color: var(--text-main);
  word-break: break-all;
}
</style>
