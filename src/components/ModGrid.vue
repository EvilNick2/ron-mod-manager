<script setup lang="ts">
import { store } from "../store";
import { invoke } from "@tauri-apps/api/core";
import { onMounted, ref, computed } from "vue";
import { Search, Plus, RefreshCw, X } from 'lucide-vue-next';

const searchQuery = ref("");
const activeCategory = ref("All");

const filteredMods = computed(() => {
  const sourceArray = store.currentMode === 'Online' ? store.onlineMods : store.mods;
  
  const filtered = sourceArray.filter((mod) => {
    if (store.currentMode === 'Installed') {
      if (store.activeDeploymentFilter === 'Enabled' && !mod.enabled) return false;
      if (store.activeDeploymentFilter === 'Disabled' && mod.enabled) return false;
    }

    const s = searchQuery.value.toLowerCase();
    const searchMatch = !s || mod.name.toLowerCase().includes(s) || (mod.description && mod.description.toLowerCase().includes(s));
    
    let categoryMatch = true;
    const bodyText = (mod.name + " " + (mod.description || "")).toLowerCase();
    
    if (activeCategory.value === "Maps") {
      categoryMatch = bodyText.includes("map") || bodyText.includes("level") || bodyText.includes("mission") || bodyText.includes("location") || bodyText.includes("environment");
    } else if (activeCategory.value === "Weapons") {
      categoryMatch = bodyText.includes("weapon") || bodyText.includes("gun") || bodyText.includes("rifle") || bodyText.includes("pistol") || bodyText.includes("optic") || bodyText.includes("attachment");
    } else if (activeCategory.value === "AI") {
      categoryMatch = bodyText.includes(" ai ") || bodyText.includes("behavior") || bodyText.includes("suspect") || bodyText.includes("swat") || bodyText.includes("civilian") || bodyText.includes("difficulty");
    } else if (activeCategory.value === "UI") {
      categoryMatch = bodyText.includes(" ui ") || bodyText.includes("menu") || bodyText.includes("hud") || bodyText.includes("interface") || bodyText.includes("indicator");
    }
    
    return searchMatch && categoryMatch;
  });

  if (store.currentMode === 'Installed') {
    return filtered.sort((a: any, b: any) => a.name.localeCompare(b.name));
  }

  return filtered;
});

async function loadMoreMods() {
  if (store.isFetchingOnline || !store.hasMoreOnlineMods) return;
  store.isFetchingOnline = true;
  try {
    const result: any = await invoke("search_nexus_mods", { offset: store.onlinePageOffset });
    const nodes = result?.data?.mods?.nodes || [];
    
    if (nodes.length === 0) {
      store.hasMoreOnlineMods = false;
      return;
    }
    
    const existingIds = new Set(store.onlineMods.map((m: any) => m.id));
    
    const formatted = nodes
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
      }))
      .filter((m: any) => !existingIds.has(m.id));
    
    store.onlineMods = [...store.onlineMods, ...formatted];
    store.onlinePageOffset += 20;
    if (nodes.length < 20) store.hasMoreOnlineMods = false;
  } catch (e) {
    console.error("Failed to load more mods", e);
  } finally {
    store.isFetchingOnline = false;
  }
}

onMounted(async () => {
  try {
    await refreshMods(true);
  } catch(e) {
    console.error("Failed to scan local mods:", e);
  }
});

function selectMod(mod: any) {
  store.selectedMod = mod;
}

async function toggleModRightClick(mod: any) {
  try {
    const newState = !mod.enabled;
    await invoke("toggle_mod", { modId: mod.id, enable: newState });
    mod.enabled = newState;
  } catch (e: any) {
    console.error("Right-click deploy failed:", e);
    alert("Failed to toggle mod deployment: " + e);
  }
}

async function manualPickArchive() {
  try {
    const file: string | null = await invoke("pick_mod_archive");
    if (file) {
      store.installingModPath = file;
    }
  } catch (e) {
    console.error(e);
  }
}

async function removeMod(mod: any) {
  if (!confirm(`Are you absolutely sure you want to completely remove ${mod.name}?`)) return;
  
  try {
    await invoke("delete_mod", { modId: mod.id });
    
    store.mods = store.mods.filter((m) => m.id !== mod.id);
    
    if (store.selectedMod?.id === mod.id) {
      store.selectedMod = null;
    }
  } catch (e: any) {
    console.error("Engine failed to purge mod:", e);
    alert("Purge Failure: " + e);
  }
}

async function startModUpdateFlow(mod: any) {
  const modId = mod?.nexus_mod_id || Number.parseInt(mod?.id, 10);
  if (!modId || Number.isNaN(modId)) {
    alert("This mod is missing a Nexus mod ID, so update flow can't be started");
    return;
  }

  const filesUrl = `https://www.nexusmods.com/${store.gameDomainStore}/mods/${modId}?tab=files`;
  try {
    await invoke("open_browser_url", { url: filesUrl });
    store.awaitingDropForId = String(modId);
    store.installingModPath = null;
  } catch (e: any) {
    console.error("Failed to start mod update flow:", e);
    alert("Failed to open Nexus page: " + e);
  }
}

async function refreshMods(checkForUpdates = true) {
  try {
    store.mods = await invoke("scan_local_mods");
    if (checkForUpdates) {
      await invoke("refresh_installed_mod_updates");
      store.mods = await invoke("scan_local_mods");
    }
    if (store.currentMode === 'Online') {
      store.onlineMods = [];
      store.onlinePageOffset = 0;
      store.hasMoreOnlineMods = true;
    }
  } catch(e) {
    console.error("Failed to refresh mods:", e);
  }
}

async function refreshModsFromButton() {
  await refreshMods(true);
}
</script>

<template>
  <main class="main-content">
    <header class="top-bar">
      <div class="search-container">
        <Search class="search-icon" :size="18" color="#94a3b8" />
        <input type="text" placeholder="Search mods..." class="search-bar" v-model="searchQuery" />
      </div>
      
      <button class="btn add-mod-btn" @click="manualPickArchive" title="Install from .zip, .rar, or .pak archive">
        <Plus :size="18" color="#22c55e" /> Add Mod
      </button>

      <button class="btn" @click="refreshModsFromButton" title="Refresh Mod List" style="margin-left: auto;">
        <RefreshCw :size="18" color="#3b82f6" /> Refresh
      </button>
    </header>

    <div class="mod-grid">
      <div v-if="store.currentMode === 'Online' && store.isFetchingOnline" style="grid-column: 1/-1; text-align: center; color: var(--text-muted); margin-top: 2rem;">
        <div class="spinner"></div>
        <h3 style="margin-top: 1rem;">Fetching Trending Mods from Nexus...</h3>
      </div>
    
      <div v-else-if="filteredMods.length === 0" style="grid-column: 1/-1; text-align: center; color: var(--text-muted); margin-top: 2rem;">
        <h3 v-if="store.currentMode === 'Installed' && store.mods.length === 0">No mods detected in local storage.</h3>
        <h3 v-else>No mods match your current search filters.</h3>
        <p v-if="store.currentMode === 'Installed' && store.mods.length === 0">Drop a .zip, .rar, or .pak file anywhere to install a new mod!</p>
      </div>
      
      <div 
        v-for="mod in filteredMods" 
        :key="mod.id"
        class="mod-card"
        :class="{ 
          'selected-mod': store.selectedMod?.id === mod.id,
          'enabled-mod': mod.enabled
        }"
        @click="selectMod(mod)"
        @contextmenu.prevent="toggleModRightClick(mod)"
      >
        <button
          v-if="store.currentMode === 'Installed' && mod.update_available"
          class="update-available-banner"
          @click.stop="startModUpdateFlow(mod)"
          title="Open Nexus files page and import the updated archive"
        >
          Update Available
        </button>

        <img :src="mod.thumbnail_url || 'https://images.unsplash.com/photo-1595590424283-b8f17842773f?auto=format&fit=crop&q=80&w=400&h=400'" alt="Mod Cover" class="mod-image" loading="lazy" decoding="async" />
        <div class="mod-info">
          <h3>{{ mod.name }}</h3>
        </div>
        <button v-if="!mod.is_online" class="delete-mod" @click.stop="removeMod(mod)" title="Remove Mod Completely">
          <X :size="16" color="#ffffff" />
        </button>
      </div>
      
      <button 
        v-if="store.currentMode === 'Online' && store.onlineMods.length > 0 && !store.isFetchingOnline && store.hasMoreOnlineMods" 
        class="load-more-btn" 
        @click="loadMoreMods" 
      >
        Load More Mods
      </button>
      <div v-if="store.currentMode === 'Online' && store.isFetchingOnline && store.onlineMods.length > 0" style="grid-column: 1/-1; text-align: center; padding: 2rem;">
        <div class="spinner"></div>
      </div>
    </div>

    <div class="floating-categories">
      <div class="category-pill" :class="{ active: activeCategory === 'All' }" @click="activeCategory = 'All'">All</div>
      <div class="category-pill" :class="{ active: activeCategory === 'Maps' }" @click="activeCategory = 'Maps'">Maps</div>
      <div class="category-pill" :class="{ active: activeCategory === 'Weapons' }" @click="activeCategory = 'Weapons'">Weapons</div>
      <div class="category-pill" :class="{ active: activeCategory === 'AI' }" @click="activeCategory = 'AI'">AI</div>
      <div class="category-pill" :class="{ active: activeCategory === 'UI' }" @click="activeCategory = 'UI'">UI</div>
    </div>
  </main>
</template>

<style scoped>
.main-content {
  background-color: var(--bg-darker);
  display: flex;
  flex-direction: column;
  position: relative;
  overflow: hidden;
  background-image: radial-gradient(var(--bg-card) 1px, transparent 1px);
  background-size: 20px 20px;
}

.top-bar {
  padding: 1.5rem 2rem;
  display: flex;
  align-items: center;
}

.search-container {
  position: relative;
  width: 100%;
  max-width: 500px;
}

.search-bar {
  width: 100%;
  box-sizing: border-box;
  background: rgba(21, 24, 30, 0.8);
  border: 1px solid var(--border-light);
  padding: 0.85rem 1rem 0.85rem 2.8rem;
  border-radius: 99px;
  color: var(--text-main);
  outline: none;
  font-size: 0.95rem;
  transition: all 0.2s;
  backdrop-filter: blur(4px);
}

.search-bar:focus {
  border-color: var(--accent-primary);
  background: rgba(21, 24, 30, 1);
}

.search-icon {
  position: absolute;
  left: 1.2rem;
  top: 50%;
  transform: translateY(-50%);
  color: var(--text-muted);
  font-size: 1.1rem;
}

.btn {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.85rem 1.25rem;
  margin-left: 1.5rem;
  border-radius: 8px;
  font-weight: 600;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 0.5rem;
  transition: all 0.2s;
}

.btn:hover {
  background: var(--bg-hover);
  border-color: var(--accent-primary);
  color: var(--accent-primary);
}

.add-mod-btn {
  margin-left: 1.5rem;
}

.mod-grid {
  padding: 1rem 2rem 6rem 2rem;
  flex: 1 1 0;
  min-height: 0;
  overflow-y: auto;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 1.5rem;
  align-content: start;
}

.mod-card {
  background: var(--bg-dark);
  border-radius: 12px;
  overflow: hidden;
  position: relative;
  border: 2px solid var(--border-dark);
  transition: transform 0.2s, box-shadow 0.2s, border-color 0.2s;
  cursor: pointer;
  box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.5);
  height: 240px;
  contain: layout style;
  will-change: transform;
  content-visibility: auto;
  contain-intrinsic-size: 240px 240px;
}

.update-available-banner {
  position: absolute;
  top: 0.6rem;
  left: 0.6rem;
  z-index: 3;
  border: 1px solid rgba(16, 185, 129, 0.9);
  background: rgba(9, 78, 59, 0.9);
  color: #a7f3d0;
  font-size: 0.75rem;
  font-weight: 800;
  text-transform: uppercase;
  letter-spacing: 0.4px;
  border-radius: 999px;
  padding: 0.35rem 0.65rem;
  cursor: pointer;
}

.update-available-banner:hover {
  background: rgba(5, 150, 105, 0.9);
  color: #ecfdf5;
}

.mod-card:hover {
  transform: translateY(-4px);
  box-shadow: 0 12px 20px -4px rgba(0, 0, 0, 0.8);
  border-color: var(--border-light);
  z-index: 2;
}

.mod-card.enabled-mod {
  border-color: #22c55e;
  box-shadow: 0 0 15px rgba(34, 197, 94, 0.2);
}

.mod-card.selected-mod {
  outline: 2px solid var(--accent-primary);
  outline-offset: 3px;
}

.mod-image {
  width: 100%;
  height: 100%;
  object-fit: cover;
  filter: brightness(0.85);
  transition: filter 0.3s;
}

.spinner {
  width: 40px;
  height: 40px;
  border: 4px solid rgba(255, 255, 255, 0.1);
  border-left-color: var(--accent-primary);
  border-radius: 50%;
  animation: spin 1s linear infinite;
  display: inline-block;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.load-more-btn {
  grid-column: 1 / -1;
  margin: 3rem auto;
  padding: 1rem 3rem;
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  border-radius: 8px;
  font-weight: 700;
  cursor: pointer;
  transition: all 0.2s;
  text-transform: uppercase;
  letter-spacing: 1px;
}

.load-more-btn:hover {
  background: var(--accent-primary);
  border-color: var(--accent-primary);
  transform: translateY(-2px);
  box-shadow: 0 4px 12px rgba(230,29,54,0.3);
}

.mod-card:hover .mod-image {
  filter: brightness(1.1);
}

.mod-info {
  position: absolute;
  bottom: 0;
  left: 0;
  right: 0;
  padding: 1.5rem 1rem 1rem;
  background: linear-gradient(to top, rgba(0,0,0,0.95) 0%, rgba(0,0,0,0.6) 60%, transparent 100%);
}

.mod-info h4 {
  margin: 0;
  font-size: 1.05rem;
  color: white;
  text-shadow: 0 1px 2px rgba(0,0,0,0.9);
}

.delete-mod {
  position: absolute;
  top: 0.5rem;
  right: 0.5rem;
  background: rgba(0,0,0,0.6);
  color: white;
  border: none;
  width: 26px;
  height: 26px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 1.2rem;
  cursor: pointer;
  opacity: 0;
  transition: all 0.2s;
}

.mod-card:hover .delete-mod {
  opacity: 1;
}

.delete-mod:hover {
  background: #ef4444;
  transform: scale(1.1);
}

.floating-categories {
  position: absolute;
  bottom: 2rem;
  left: 50%;
  transform: translateX(-50%);
  background: #111418;
  padding: 0.4rem;
  border-radius: 99px;
  border: 1px solid var(--border-light);
  display: flex;
  gap: 0.2rem;
  box-shadow: 0 15px 30px rgba(0, 0, 0, 0.8);
  z-index: 10;
}

.category-pill {
  padding: 0.5rem 1.5rem;
  border-radius: 99px;
  font-size: 0.9rem;
  font-weight: 600;
  color: var(--text-muted);
  cursor: pointer;
  transition: all 0.2s;
  position: relative;
}

.category-pill:hover {
  color: var(--text-main);
  background: rgba(255,255,255,0.05);
}

.category-pill.active {
  background: transparent;
  color: var(--accent-primary);
}

.category-pill.active::after {
  content: '';
  position: absolute;
  bottom: -0.4rem;
  left: 1.2rem;
  right: 1.2rem;
  height: 3px;
  background-color: var(--accent-primary);
  border-radius: 3px 3px 0 0;
}
</style>
