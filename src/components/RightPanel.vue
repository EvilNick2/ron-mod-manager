<script setup lang="ts">
import { store } from "../store";
import { invoke } from "@tauri-apps/api/core";
import { ref, computed } from "vue";
import { ExternalLink, Download, PackageSearch, X, Plus, Puzzle, Trash2 } from 'lucide-vue-next';

const isToggling = ref(false);
const errorMsg = ref("");
const isImageEnlarged = ref(false);
const isAddonsExpanded = ref(true);

async function toggleActiveMod() {
  if (!store.selectedMod) return;
  errorMsg.value = "";
  isToggling.value = true;
  
  const newState = !store.selectedMod.enabled;
  
  try {
    await invoke("toggle_mod", {
      modId: store.selectedMod.id,
      enable: newState
    });
    
    store.selectedMod.enabled = newState;
    
    const target = store.mods.find(m => m.id === store.selectedMod.id);
    if (target) target.enabled = newState;
    
  } catch (e: any) {
    errorMsg.value = "Error: " + e;
  } finally {
    isToggling.value = false;
  }
}

async function downloadOnlineMod() {
  if (!store.selectedMod) return;
  
  const url = `https://www.nexusmods.com/readyornot/mods/${store.selectedMod.id}?tab=files`;
  
  try {
    await invoke("open_browser_url", { url });
    store.awaitingDropForId = store.selectedMod.id;
  } catch (e: any) {
    errorMsg.value = "Failed to launch browser: " + e;
  }
}

async function pickAddonFile() {
  if (!store.selectedMod) return;
  try {
    const file: string | null = await invoke("pick_mod_archive");
    if (file) {
      await invoke("install_addon", { modId: store.selectedMod.id, archivePath: file });
      store.mods = await invoke("scan_local_mods");
      const updated = store.mods.find((m: any) => m.id === store.selectedMod.id);
      if (updated) store.selectedMod = updated;
    }
  } catch (e: any) {
    errorMsg.value = "Failed to add addon: " + e;
  }
}

async function toggleAddon(filename: string, enable: boolean) {
  if (!store.selectedMod) return;
  try {
    await invoke("toggle_addon", { modId: store.selectedMod.id, filename, enable });
    store.mods = await invoke("scan_local_mods");
    const updated = store.mods.find((m: any) => m.id === store.selectedMod.id);
    if (updated) store.selectedMod = updated;
  } catch (e: any) {
    errorMsg.value = "Addon toggle failed: " + e;
  }
}

async function deleteAddon(filename: string) {
  if (!store.selectedMod) return;
  if (!confirm(`Remove addon "${filename}"?`)) return;
  try {
    await invoke("remove_addon", { modId: store.selectedMod.id, filename });
    store.mods = await invoke("scan_local_mods");
    const updated = store.mods.find((m: any) => m.id === store.selectedMod.id);
    if (updated) store.selectedMod = updated;
  } catch (e: any) {
    errorMsg.value = "Failed to remove addon: " + e;
  }
}

const cleanDescription = computed(() => {
  if (!store.selectedMod || !store.selectedMod.description) return "No description provided.";
  let text: string = store.selectedMod.description;
  
  text = text.replace(/<[^>]*>?/gm, '');
  text = text.replace(/\[\/?.*?\]/g, '');
  text = text.replace(/&#92;/g, '\\');
  text = text.replace(/&amp;/g, '&');
  text = text.replace(/&quot;/g, '"');
  text = text.replace(/&lt;/g, '<');
  text = text.replace(/&gt;/g, '>');
  
  return text.trim();
});
</script>

<template>
  <aside class="right-panel" v-if="store.selectedMod">
    <div class="panel-header">
      <h2>{{ store.selectedMod.name }}</h2>
      <a href="#" class="external-link" title="View on Nexus Mods" @click.prevent="invoke('open_browser_url', { url: `https://www.nexusmods.com/readyornot/mods/${store.selectedMod.id}` })">
        <ExternalLink :size="18" color="#3b82f6" />
      </a>
    </div>
    
    <img 
      :src="store.selectedMod.thumbnail_url || 'https://images.unsplash.com/photo-1579566346927-c68383817a25?auto=format&fit=crop&q=80&w=600&h=400'" 
      alt="Detail" 
      class="detail-banner" 
      @click="isImageEnlarged = true"
      title="Click to Enlarge"
    />
    
    <div class="mod-description-panel">
      {{ cleanDescription }}
    </div>

    <div class="detail-content">
      <div class="detail-row">
        <span class="detail-label">Author</span>
        <span class="detail-value">{{ store.selectedMod.author }}</span>
      </div>
      <div class="detail-row" v-if="store.selectedMod.version">
        <span class="detail-label">Version</span>
        <span class="detail-value">{{ store.selectedMod.version }}</span>
      </div>
      
      <template v-if="store.selectedMod.is_online">
        <div class="detail-row">
          <span class="detail-label">Downloads</span>
          <span class="detail-value">{{ (store.selectedMod.download_count || 0).toLocaleString() }}</span>
        </div>
        <div class="detail-row">
          <span class="detail-label">Endorsements</span>
          <span class="detail-value">{{ (store.selectedMod.endorsement_count || 0).toLocaleString() }}</span>
        </div>
        <div class="detail-row" v-if="store.selectedMod.updated_at">
          <span class="detail-label">Last Updated</span>
          <span class="detail-value">{{ new Date(store.selectedMod.updated_at).toLocaleDateString() }}</span>
        </div>
        <div class="detail-row" v-if="store.selectedMod.created_at">
          <span class="detail-label">Created</span>
          <span class="detail-value">{{ new Date(store.selectedMod.created_at).toLocaleDateString() }}</span>
        </div>
      </template>
      
      <div class="detail-row" v-if="!store.selectedMod.is_online">
        <span class="detail-label">Category</span>
        <span class="detail-value">Weapons</span>
      </div>
      
      <div v-if="errorMsg" class="error-msg">{{ errorMsg }}</div>

      <div class="action-footer" style="margin-top: 1rem;">
        <button 
          v-if="store.selectedMod.is_online"
          class="deploy-btn" 
          style="border-color: #3b82f6; background: rgba(59, 130, 246, 0.1);"
          @click="downloadOnlineMod"
        >
          <Download :size="18" color="#3b82f6" />
          Download via Browser
        </button>
        
        <button 
          v-else
          class="deploy-btn" 
          :class="{ 'is-enabled': store.selectedMod.enabled }"
          @click="toggleActiveMod"
          :disabled="isToggling"
        >
          <span class="status-indicator"></span>
          {{ isToggling ? 'Syncing...' : (store.selectedMod.enabled ? 'Enabled (Deployed)' : 'Disabled') }}
        </button>
      </div>

      <div v-if="!store.selectedMod.is_online" class="addons-section">
        <div class="addons-header" @click="isAddonsExpanded = !isAddonsExpanded">
          <Puzzle :size="16" color="#f59e0b" />
          <span>Optional Addons</span>
          <span class="addon-count" v-if="store.selectedMod.addons?.length">{{ store.selectedMod.addons.length }}</span>
          <span class="expand-arrow" :class="{ expanded: isAddonsExpanded }">▸</span>
        </div>
        
        <div v-if="isAddonsExpanded" class="addons-list">
          <div v-if="!store.selectedMod.addons?.length" class="no-addons">
            No addons installed
          </div>
          <div v-for="addon in store.selectedMod.addons" :key="addon.filename" class="addon-row" @contextmenu.prevent="toggleAddon(addon.filename, !addon.enabled)">
            <button 
              class="addon-toggle" 
              :class="{ 'addon-enabled': addon.enabled }"
              @click="toggleAddon(addon.filename, !addon.enabled)"
              :title="addon.enabled ? 'Disable' : 'Enable'"
            >
              <span class="addon-dot"></span>
            </button>
            <span class="addon-name">{{ addon.filename.replace('.pak', '') }}</span>
            <button class="addon-delete" @click="deleteAddon(addon.filename)" title="Remove addon">
              <Trash2 :size="14" color="#ef4444" />
            </button>
          </div>
          <button class="add-addon-btn" @click="pickAddonFile">
            <Plus :size="14" color="#22c55e" /> Add Addon
          </button>
        </div>
      </div>
    </div>
  </aside>
  
  <aside class="right-panel empty-panel" v-else>
    <div style="text-align:center; color: var(--text-muted); opacity: 0.5;">
      <PackageSearch :size="48" color="#94a3b8" style="margin-bottom: 1rem;" />
      <p>Select a mod from the grid to view details.</p>
    </div>
  </aside>

  <teleport to="body">
    <transition name="fade">
      <div v-if="isImageEnlarged && store.selectedMod" class="lightbox-overlay" @click="isImageEnlarged = false">
        <button class="lightbox-close">
          <X :size="24" color="#ffffff" />
        </button>
        <img :src="store.selectedMod.thumbnail_url || 'https://images.unsplash.com/photo-1579566346927-c68383817a25?auto=format&fit=crop&q=80&w=1200&h=800'" class="lightbox-img" @click.stop />
      </div>
    </transition>
  </teleport>
</template>

<style scoped>
.right-panel {
  background-color: var(--bg-dark);
  border-left: 1px solid var(--border-dark);
  display: flex;
  flex-direction: column;
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
  word-wrap: break-word;
  overflow-wrap: break-word;
}

.external-link {
  color: var(--accent-primary);
  text-decoration: none;
  font-weight: bold;
  font-size: 1.2rem;
}

.detail-banner {
  width: 100%;
  height: 220px;
  object-fit: cover;
  border-bottom: 2px solid var(--border-light);
  cursor: zoom-in;
  transition: filter 0.2s;
}

.detail-banner:hover {
  filter: brightness(1.15);
}

.detail-content {
  padding: 1.5rem;
  display: flex;
  flex-direction: column;
  gap: 1.2rem;
}

.detail-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid var(--border-light);
  padding-bottom: 0.75rem;
}

.detail-label {
  color: var(--text-muted);
  font-size: 0.8rem;
  font-weight: 700;
  text-transform: uppercase;
}

.detail-value {
  color: var(--text-main);
  font-size: 0.95rem;
  font-weight: 600;
}

.mod-description-panel {
  padding: 1.25rem 1.5rem;
  color: var(--text-main);
  font-size: 0.95rem;
  line-height: 1.5;
  text-align: left;
  border-bottom: 1px solid var(--border-dark);
  max-height: 200px;
  overflow-y: auto;
}

/* Beautiful custom scrollbar for the description */
.mod-description-panel::-webkit-scrollbar {
  width: 6px;
}
.mod-description-panel::-webkit-scrollbar-track {
  background: var(--bg-dark);
}
.mod-description-panel::-webkit-scrollbar-thumb {
  background: var(--border-light);
  border-radius: 4px;
}
.mod-description-panel::-webkit-scrollbar-thumb:hover {
  background: var(--text-muted);
}

.empty-panel {
  justify-content: center;
  align-items: center;
}

.deploy-btn {
  width: 100%;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 1rem;
  border-radius: 8px;
  font-size: 1.05rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 1px;
  cursor: pointer;
  transition: all 0.2s;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.75rem;
}

.deploy-btn:hover {
  background: var(--bg-hover);
}

.status-indicator {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  background: var(--text-muted);
  box-shadow: 0 0 10px rgba(255,255,255,0.1);
  transition: all 0.3s;
}

.deploy-btn.is-enabled {
  border-color: #10b981;
  background: rgba(16, 185, 129, 0.1);
}

.deploy-btn.is-enabled .status-indicator {
  background: #10b981;
  box-shadow: 0 0 15px rgba(16, 185, 129, 0.8);
}

.error-msg {
  color: #fb7185;
  font-size: 0.85rem;
  line-height: 1.4;
  margin-top: 0.5rem;
}

/* Lightbox Modal Styles */
.lightbox-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(5, 5, 8, 0.9);
  backdrop-filter: blur(10px);
  z-index: 99999;
  display: flex;
  justify-content: center;
  align-items: center;
  cursor: zoom-out;
}

.lightbox-img {
  max-width: 90vw;
  max-height: 90vh;
  object-fit: contain;
  border-radius: 8px;
  box-shadow: 0 20px 50px rgba(0,0,0,0.8);
  border: 1px solid var(--border-light);
  cursor: default;
}

.lightbox-close {
  position: absolute;
  top: 1.5rem;
  right: 2rem;
  background: rgba(0,0,0,0.5);
  border: 1px solid var(--border-light);
  color: white;
  width: 44px;
  height: 44px;
  border-radius: 50%;
  font-size: 2rem;
  display: flex;
  justify-content: center;
  align-items: center;
  cursor: pointer;
  transition: all 0.2s;
}

.lightbox-close:hover {
  background: var(--accent-primary);
  transform: scale(1.1);
}

.addons-section {
  margin-top: 1rem;
  padding: 0 1.5rem 1.5rem;
}

.addons-header {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  cursor: pointer;
  color: var(--text-muted);
  font-size: 0.85rem;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  padding: 0.5rem 0;
  user-select: none;
}

.addon-count {
  background: rgba(245, 158, 11, 0.2);
  color: #f59e0b;
  font-size: 0.7rem;
  padding: 0.25rem 0.6rem;
  border-radius: 99px;
  font-weight: 700;
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 1.25rem;
  line-height: 1;
}

.expand-arrow {
  margin-left: auto;
  transition: transform 0.2s;
  font-size: 0.8rem;
}

.expand-arrow.expanded {
  transform: rotate(90deg);
}

.addons-list {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  margin-top: 0.25rem;
}

.no-addons {
  color: var(--text-muted);
  font-size: 0.8rem;
  opacity: 0.6;
  padding: 0.25rem 0;
}

.addon-row {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  padding: 0.35rem 0.5rem;
  border-radius: 6px;
  transition: background 0.15s;
}

.addon-row:hover {
  background: rgba(255, 255, 255, 0.03);
}

.addon-toggle {
  background: none;
  border: none;
  cursor: pointer;
  padding: 0.2rem;
  display: flex;
  align-items: center;
}

.addon-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: var(--text-muted);
  transition: all 0.2s;
}

.addon-enabled .addon-dot {
  background: #22c55e;
  box-shadow: 0 0 8px rgba(34, 197, 94, 0.6);
}

.addon-name {
  flex: 1;
  font-size: 0.85rem;
  color: var(--text-main);
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.addon-delete {
  background: none;
  border: none;
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.15s;
  padding: 0.2rem;
  display: flex;
  align-items: center;
}

.addon-row:hover .addon-delete {
  opacity: 1;
}

.addon-delete:hover {
  transform: scale(1.15);
}

.add-addon-btn {
  background: transparent;
  border: 1px dashed var(--border-light);
  color: var(--text-muted);
  padding: 0.4rem;
  border-radius: 6px;
  font-size: 0.8rem;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 0.3rem;
  margin-top: 0.25rem;
  transition: all 0.2s;
}

.add-addon-btn:hover {
  border-color: #22c55e;
  background: rgba(34, 197, 94, 0.05);
  color: var(--text-main);
}
</style>
