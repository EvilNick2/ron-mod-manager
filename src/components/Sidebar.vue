<script setup lang="ts">
import { onMounted } from 'vue';
import { invoke } from "@tauri-apps/api/core";
import { store } from "../store";
import { 
  Package, 
  Globe, 
  Settings, 
  Download, 
  Upload, 
  Save, 
  X,
  CheckCircle,
  CircleOff,
  Play
} from 'lucide-vue-next';

async function loginNexus() {
  try {
    const apiKey: string = await invoke("authenticate_nexus");
    store.apiKeyStore = apiKey;
  } catch (error) {
    console.error(error);
  }
}

async function loadPresets() {
  try {
    store.presets = await invoke("load_presets") as Record<string, string[]>;
    if (store.activePresetName && !store.presets[store.activePresetName]) {
      store.activePresetName = "";
    }
  } catch (e) {
    console.error("Failed to load presets:", e);
  }
}

async function saveCurrentPreset() {
  const name = prompt("Name this preset:");
  if (!name || !name.trim()) return;
  const enabledIds = store.mods.filter((m: any) => m.enabled).map((m: any) => m.id);
  if (enabledIds.length === 0) {
    alert("No mods are currently enabled to save.");
    return;
  }
  try {
    await invoke("save_preset", { name: name.trim(), modIds: enabledIds });
    await loadPresets();
  } catch (e) {
    console.error("Failed to save preset:", e);
  }
}

async function applyPreset(name: string) {
  const modIds = store.presets[name];
  if (!modIds) return;
  try {
    await invoke("apply_preset", { name, modIds });
    const refreshed: any[] = await invoke("scan_local_mods");
    store.mods = refreshed;
    store.activePresetName = name;
  } catch (e) {
    console.error("Failed to apply preset:", e);
    alert("Failed to apply preset: " + e);
  }
}

async function exportPreset(name: string) {
  try {
    await invoke("export_preset", { presetName: name });
    alert(`Successfully exported preset: ${name}`);
  } catch (e: any) {
    if (e !== "Export cancelled") {
      console.error("Failed to export preset:", e);
      alert("Failed to export preset: " + e);
    }
  }
}

async function importPreset() {
  try {
    const importedName: string = await invoke("import_preset");
    alert(`Successfully imported preset: ${importedName}`);
    await loadPresets();
    
    await applyPreset(importedName);
    
    const refreshed: any[] = await invoke("scan_local_mods");
    store.mods = refreshed;
  } catch (e: any) {
    if (e !== "Import cancelled") {
      console.error("Failed to import preset:", e);
      alert("Failed to import preset: " + e);
    }
  }
}

async function removePreset(name: string) {
  try {
    await invoke("delete_preset", { name });
    if (store.activePresetName === name) {
      store.activePresetName = "";
      const config: any = await invoke("load_config");
      config.active_preset = null;
      await invoke("save_config", { config });
    }
    await loadPresets();
  } catch (e) {
    console.error("Failed to delete preset:", e);
  }
}

async function bulkToggleMods(enable: boolean) {
  store.isFetchingOnline = true;
  try {
    await invoke("toggle_all_mods", { enable });
    const refreshed: any[] = await invoke("scan_local_mods");
    store.mods = refreshed;
  } catch (e: any) {
    console.error("Bulk toggle failed:", e);
    alert("Bulk Action Failed: " + e);
  } finally {
    store.isFetchingOnline = false;
  }
}

onMounted(() => {
  loadPresets();
});

async function fetchOnlineMods() {
  if (store.onlineMods.length > 0) return;
  
  if (!store.apiKeyStore) {
    alert("You must log into Nexus Mods to browse online mods.");
    return;
  }

  store.isFetchingOnline = true;
  store.onlinePageOffset = 0;
  store.hasMoreOnlineMods = true;
  try {
    const result: any = await invoke("search_nexus_mods", { offset: 0 });
    console.log("[ONLINE] Raw search result:", JSON.stringify(result).substring(0, 500));
    const nodes = result?.data?.mods?.nodes || [];
    console.log("[ONLINE] Parsed nodes count:", nodes.length);
    if (nodes.length === 0) {
      console.warn("[ONLINE] No nodes found. Result structure:", Object.keys(result || {}));
      if (result?.errors) console.error("[ONLINE] GraphQL errors:", result.errors);
    }
    
    store.onlineMods = nodes
      .filter((m: any) => m.modId && m.name)
      .map((m: any) => ({
        id: m.modId.toString(),
        name: m.name,
        description: m.summary || "",
        author: m.author || "Unknown",
        version: m.version || "",
        is_dir: false,
        thumbnail_url: m.pictureUrl || "",
        enabled: false,
        is_online: true,
        download_count: m.downloads || 0,
        endorsement_count: m.endorsements || 0,
        updated_at: m.updatedAt || "",
        created_at: m.createdAt || ""
      }));
    
    console.log("[ONLINE] Mapped mods count:", store.onlineMods.length);
    store.onlinePageOffset = 20;
    if (nodes.length < 20) store.hasMoreOnlineMods = false;
  } catch(e) {
     console.error("Network Fetch failure:", e);
  } finally {
     store.isFetchingOnline = false;
  }
}

async function launchGame() {
  try {
    console.log("[LAUNCH] Sending command to the backend to launch game...");
    await invoke("launch_game");
  } catch (e) {
    console.error("Failed to launch game:", e);
    alert("Failed to launch game: " + e);
  }
}
</script>

<template>
  <aside class="sidebar">
    <div class="logo-area">
      <h1 class="logo-title">RoN</h1>
      <p class="logo-subtitle">Mod Manager</p>
    </div>

    <div class="sidebar-section">
      <div class="mode-toggles">
        <button 
          class="mode-btn" 
          :class="{ active: store.currentMode === 'Installed' }"
          @click="store.currentMode = 'Installed'"
        >
          <Package :size="18" color="#3b82f6" /> Installed
        </button>
        <button 
          class="mode-btn online-btn" 
          :class="{ active: store.currentMode === 'Online' }"
          @click="store.currentMode = 'Online'; fetchOnlineMods()"
        >
          <Globe :size="18" color="#10b981" /> Online
        </button>
      </div>
    </div>

    <div class="sidebar-section">
      <h3 class="section-label">Filter</h3>
      <div class="filter-toggles">
        <button class="filter-btn" :class="{ active: store.activeDeploymentFilter === 'All' }" @click="store.activeDeploymentFilter = 'All'">All</button>
        <button class="filter-btn" :class="{ active: store.activeDeploymentFilter === 'Enabled' }" @click="store.activeDeploymentFilter = 'Enabled'">Enabled</button>
        <button class="filter-btn" :class="{ active: store.activeDeploymentFilter === 'Disabled' }" @click="store.activeDeploymentFilter = 'Disabled'">Disabled</button>
      </div>
      
      <div class="bulk-actions" v-if="store.currentMode === 'Installed'">
        <button class="bulk-btn" @click="bulkToggleMods(true)">
          <CheckCircle :size="14" color="#22c55e" /> Enable All
        </button>
        <button class="bulk-btn" @click="bulkToggleMods(false)">
          <CircleOff :size="14" color="#ef4444" /> Disable All
        </button>
      </div>
    </div>

    <div class="sidebar-section" v-if="store.currentMode === 'Installed'">
      <h3 class="section-label">Presets</h3>
      <div class="preset-list" v-if="Object.keys(store.presets).length > 0">
        <div class="preset-item" v-for="(ids, name) in store.presets" :key="name">
          <button
            class="preset-apply-btn"
            :class="{ active: store.activePresetName === name }"
            @click="applyPreset(name as string)"
            :title="`Apply ${name} (${ids.length} mods)`"
          >
            {{ name }}
            <span class="preset-count">{{ ids.length }}</span>
          </button>
          <button class="preset-action-btn" @click="exportPreset(name as string)" title="Export Preset to .zip">
            <Upload :size="14" color="#f59e0b" />
          </button>
          <button class="preset-delete-btn preset-action-btn" @click="removePreset(name as string)" title="Delete Preset">
            <X :size="14" color="#ef4444" />
          </button>
        </div>
      </div>
      <p v-else class="preset-empty">No presets saved yet.</p>
      
      <div class="preset-buttons" style="display: flex; gap: 0.5rem;">
        <button class="full-btn secondary-btn preset-save-btn" @click="saveCurrentPreset" style="flex: 1;">
          <Save :size="16" color="#3b82f6" /> Save
        </button>
        <button class="full-btn secondary-btn preset-save-btn" @click="importPreset" style="flex: 1;" title="Import from .zip">
          <Download :size="16" color="#3b82f6" /> Import
        </button>
      </div>
    </div>

    <div class="sidebar-section util-buttons bottom-section">
      <button class="full-btn launch-btn" @click="launchGame">
        <Play :size="18" fill="currentColor" /> Launch Game
      </button>

      <button v-if="!store.apiKeyStore" class="full-btn nexus-login" @click="loginNexus">
        Log in to Nexus Mods
      </button>
      <button v-else class="full-btn nexus-linked">
        Nexus Mods Linked
      </button>
      
      <button class="full-btn secondary-btn settings-btn" @click="store.isSettingsOpen = true">
        <Settings :size="18" color="#94a3b8" /> Settings
      </button>
    </div>
  </aside>
</template>

<style scoped>
.sidebar {
  background-color: var(--bg-dark);
  border-right: 1px solid var(--border-dark);
  display: flex;
  flex-direction: column;
  padding: 1.5rem 1rem;
}

.logo-area {
  margin-bottom: 2rem;
  padding-left: 0.5rem;
}

.logo-title {
  margin: 0;
  font-size: 2.2rem;
  font-weight: 900;
  letter-spacing: 2px;
  color: #ffffff;
}

.logo-subtitle {
  margin: 0;
  font-size: 0.85rem;
  font-weight: 700;
  color: var(--accent-primary);
  text-transform: uppercase;
  letter-spacing: 1.5px;
}

.sidebar-section {
  margin-bottom: 2rem;
}

.section-label {
  font-size: 0.75rem;
  text-transform: uppercase;
  color: var(--text-muted);
  font-weight: 700;
  margin-bottom: 0.75rem;
  padding-left: 0.5rem;
}

.mode-toggles {
  display: flex;
  gap: 0.5rem;
}

.mode-btn {
  flex: 1;
  background: transparent;
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.65rem 0.5rem;
  border-radius: 8px;
  font-weight: 600;
  font-size: 0.85rem;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.4rem;
  transition: all 0.2s ease;
  outline: none;
}

.mode-btn:hover {
  background: var(--bg-hover);
}

.mode-btn.active {
  background: var(--accent-primary);
  color: var(--text-dark);
  border-color: var(--accent-primary);
}

.filter-toggles {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.filter-btn {
  background: transparent;
  border: none;
  color: var(--text-muted);
  text-align: left;
  padding: 0.5rem 0.75rem;
  border-radius: 6px;
  font-size: 0.85rem;
  cursor: pointer;
  transition: all 0.2s;
  outline: none;
}

.filter-btn:hover {
  background: var(--bg-hover);
  color: var(--text-main);
}

.filter-btn.active {
  background: var(--bg-card);
  color: var(--accent-primary);
  font-weight: 600;
}

.bulk-actions {
  display: flex;
  gap: 0.5rem;
  margin-top: 0.75rem;
  padding: 0 0.5rem;
}

.bulk-btn {
  flex: 1;
  background: transparent;
  border: 1px solid var(--border-light);
  color: var(--text-muted);
  padding: 0.4rem;
  border-radius: 6px;
  font-size: 0.75rem;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.3rem;
  transition: all 0.2s;
}

.bulk-btn:hover {
  background: var(--bg-hover);
  color: var(--text-main);
  border-color: var(--border-light);
}

.bottom-section {
  margin-top: auto;
  margin-bottom: 0;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.full-btn {
  background: transparent;
  color: var(--text-main);
  border: 1px solid var(--border-light);
  padding: 0.75rem;
  border-radius: 8px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.2s;
  outline: none;
}

.full-btn.secondary-btn:hover {
  background: var(--bg-hover);
}

.settings-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
}

.nexus-login {
  background: #2563eb;
  border-color: #3b82f6;
  color: white;
}

.nexus-login:hover {
  background: #1d4ed8;
}

.nexus-linked {
  background: transparent;
  border-color: #10b981;
  color: #10b981;
  pointer-events: none;
}

.launch-btn {
  background: linear-gradient(135deg, #22c55e 0%, #16a34a 100%);
  border: none;
  color: white;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.6rem;
  box-shadow: 0 4px 15px rgba(34, 197, 94, 0.3);
  margin-bottom: 0.5rem;
}

.launch-btn:hover {
  transform: translateY(-2px);
  box-shadow: 0 6px 20px rgba(34, 197, 94, 0.4);
  background: linear-gradient(135deg, #2ae06b 0%, #19bd56 100%);
}

.launch-btn:active {
  transform: translateY(0);
}

/* Presets */
.preset-list {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  margin-bottom: 0.75rem;
}

.preset-item {
  display: flex;
  gap: 0.4rem;
}

.preset-apply-btn {
  flex: 1;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.5rem 0.75rem;
  border-radius: 6px;
  font-size: 0.8rem;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: space-between;
  transition: border-color 0.2s, background 0.2s;
}

.preset-apply-btn:hover {
  border-color: var(--accent-primary);
  background: var(--bg-hover);
}

.preset-apply-btn.active {
  border-color: #22c55e;
  box-shadow: 0 0 15px rgba(34, 197, 94, 0.2);
  background: rgba(34, 197, 94, 0.08);
}

.preset-apply-btn.active .preset-count {
  color: #22c55e;
}

.preset-count {
  background: var(--bg-hover);
  color: var(--text-muted);
  font-size: 0.7rem;
  padding: 0.15rem 0.5rem;
  border-radius: 99px;
}

.preset-action-btn {
  background: transparent;
  border: 1px solid var(--border-light);
  color: var(--text-muted);
  width: 30px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 1rem;
  transition: all 0.2s;
  display: flex;
  align-items: center;
  justify-content: center;
}

.preset-action-btn:hover {
  border-color: var(--accent-primary);
  color: var(--text-main);
  background: var(--bg-hover);
}

.preset-delete-btn:hover {
  border-color: #ef4444;
  color: #ef4444;
  background: rgba(239, 68, 68, 0.1);
}

.preset-empty {
  color: var(--text-muted);
  font-size: 0.8rem;
  padding: 0.5rem;
  margin: 0 0 0.75rem;
}

.preset-save-btn {
  font-size: 0.8rem;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
}
</style>
