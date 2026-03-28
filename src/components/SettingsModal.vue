<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { store } from "../store";
import { onMounted, ref } from "vue";
import { X, Globe, Crosshair, Info } from 'lucide-vue-next';

const activeTab = ref("Global");
const pendingGamePath = ref("");
const pendingLaunchOptions = ref("");

onMounted(async () => {
  try {
    store.defaultModStoragePath = await invoke("get_default_storage_path");
    
    const config: any = await invoke("load_config");
    if (config.mod_storage_path) {
      store.modStoragePathStore = config.mod_storage_path;
    }
    
    const key: string | null = await invoke("get_api_key");
    if (key) {
      store.apiKeyStore = key;
    }

    if (config.game_domain) {
      store.gameDomainStore = config.game_domain;
    }

    pendingGamePath.value = store.gamePathStore;
    pendingLaunchOptions.value = store.launchOptionsStore;
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

async function openGamePath() {
  try {
    await invoke("open_game_path");
  } catch (e) {
    console.error("Failed to open game path:", e);
    alert("Could not open folder: " + e);
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

async function saveGameConfig() {
  try {
    store.gamePathStore = pendingGamePath.value;
    store.launchOptionsStore = pendingLaunchOptions.value;
    
    const config: any = await invoke("load_config");
    config.game_path = pendingGamePath.value;
    config.launch_options = pendingLaunchOptions.value;
    
    await invoke("save_config", { config });
  } catch (e) {
    console.error("Failed to save game config", e);
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
      <button class="close-btn" @click="store.isSettingsOpen = false">
        <X :size="20" color="#94a3b8" />
      </button>
      <h2 class="modal-title">Settings</h2>
      
      <div class="settings-tabs">
        <button class="tab-btn" :class="{ active: activeTab === 'Global' }" @click="activeTab = 'Global'">
          <Globe :size="18" color="#3b82f6" /> Global
        </button>
        <button class="tab-btn" :class="{ active: activeTab === 'Game' }" @click="activeTab = 'Game'">
          <Crosshair :size="18" :color="activeTab === 'Game' ? '#ffffff' : '#ef4444'" /> Game Settings
        </button>
      </div>

      <div class="settings-content">
        <template v-if="activeTab === 'Global'">
          <div class="settings-section col-span-full">
            <h3>Local Mod Storage Location <Info :size="14" color="#3b82f6" class="info-icon" /></h3>
            <p class="settings-desc">Select the master folder where mods are saved. A `ReadyOrNot` child folder is generated here automatically.</p>
            <div class="path-picker-row">
              <button class="action-btn" @click="pickStorageFolder">Change</button>
              <input type="text" readonly :value="store.modStoragePathStore || store.defaultModStoragePath" placeholder="Default System Storage..." class="path-input" />
            </div>
          </div>
          
          <div class="settings-section">
            <h3>Nexus Mods API Configuration <Info :size="14" color="#3b82f6" class="info-icon" /></h3>
            <p class="settings-desc" style="margin-bottom: 1rem;">Your API Key is actively stored and securely managed. Re-login from sidebar to update.</p>
            <div class="api-key-box">
              <code v-if="store.apiKeyStore">{{ store.apiKeyStore.substring(0, 15) }}...*****************</code>
              <code v-else>Authentication is completely missing.</code>
            </div>
          </div>

          <div class="settings-section">
            <h3>Modal Background Appearance <Info :size="14" color="#3b82f6" class="info-icon" /></h3>
            <p class="settings-desc" style="margin-bottom: 1rem;">Toggle the cinematic gaussian blur effect when overlay menus are open.</p>
            <div class="settings-tabs" style="margin-bottom: 0;">
              <button class="tab-btn" :class="{ active: store.isBlurEnabled }" @click="toggleBlur(true)">Blur Enabled</button>
              <button class="tab-btn" :class="{ active: !store.isBlurEnabled }" @click="toggleBlur(false)">Blur Disabled</button>
            </div>
          </div>
        </template>

        <template v-if="activeTab === 'Game'">
          <div class="settings-section col-span-full">
            <div class="field-group">
              <label>Game Installation Path <Info :size="14" class="info-icon" /></label>
              <p class="field-desc">Select the folder where the game is installed.</p>
              <div class="path-picker-row">
                <input type="text" v-model="pendingGamePath" placeholder="C:\Program Files (x86)\... or /home/..." class="path-input" />
                <div class="path-actions">
                  <button class="secondary-action-btn" @click="openGamePath" title="Open game folder">Open</button>
                  <button class="action-btn" @click="manualPickGame">Browse...</button>
                </div>
              </div>
            </div>


            <div class="field-group">
              <label>Steam Launch Options <Info :size="14" class="info-icon" /></label>
              <p class="field-desc">Arguments like `-dx12`, `-vulkan`, or `-windowed`.</p>
              <input type="text" v-model="pendingLaunchOptions" placeholder="-dx12 -vulkan etc..." class="path-input" />
            </div>

            <div class="settings-footer">
              <button 
                class="save-btn" 
                @click="saveGameConfig" 
                :disabled="pendingGamePath === store.gamePathStore && pendingLaunchOptions === store.launchOptionsStore"
              >
                Save Configuration
              </button>
            </div>
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

.field-group {
  margin-bottom: 1.5rem;
  display: flex;
  flex-direction: column;
}

.field-group label {
  color: var(--text-main);
  font-size: 0.95rem;
  font-weight: 700;
  margin-bottom: 0.4rem;
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.field-desc {
  color: var(--text-muted);
  font-size: 0.8rem;
  margin-bottom: 0.75rem;
}

.grid-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 1.5rem;
}

.action-btn {
  background: var(--accent-primary);
  border: 1px solid var(--accent-primary);
  color: white;
  padding: 0.75rem 1.5rem;
  border-radius: 8px;
  cursor: pointer;
  font-weight: 700;
  transition: all 0.2s;
}

.action-btn:hover {
  background: var(--accent-hover);
  transform: translateY(-1px);
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

.secondary-action-btn:hover {
  background: var(--bg-card);
  border-color: var(--text-muted);
}

.path-picker-row {
  display: flex;
  gap: 0.75rem;
  align-items: center;
}

.path-actions {
  display: flex;
  gap: 0.5rem;
}

.path-input {
  flex: 1;
  background: rgba(0,0,0,0.4);
  color: var(--text-main);
  border: 1px solid var(--border-light);
  border-radius: 8px;
  padding: 0.75rem 1rem;
  font-family: 'JetBrains Mono', monospace;
  font-size: 0.85rem;
  outline: none;
  transition: border-color 0.2s, background 0.2s;
}

.path-input:focus {
  border-color: var(--accent-primary);
  background: rgba(0,0,0,0.6);
}

.settings-footer {
  margin-top: 1rem;
  display: flex;
  justify-content: flex-end;
}

.save-btn {
  background: linear-gradient(135deg, var(--accent-primary) 0%, #b91c1c 100%);
  color: white;
  border: none;
  padding: 0.75rem 2.5rem;
  border-radius: 8px;
  font-weight: 800;
  font-size: 0.95rem;
  cursor: pointer;
  transition: all 0.2s;
  box-shadow: 0 4px 15px rgba(230, 29, 54, 0.3);
}

.save-btn:hover:not(:disabled) {
  transform: translateY(-2px);
  box-shadow: 0 6px 20px rgba(230, 29, 54, 0.4);
  filter: brightness(1.1);
}

.save-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
  filter: grayscale(1);
  box-shadow: none;
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

h3 {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.info-icon {
  color: var(--accent-primary);
  opacity: 0.8;
  cursor: help;
}

.tab-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
}
</style>
