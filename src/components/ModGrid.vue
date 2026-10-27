<script setup lang="ts">
import { store } from "../store";
import { sectionMods, LOOSE_ID, type Section } from "../modSections";
import {
  applyPack, refreshMods as rescanMods, publishPack, copyInviteLink, progressLine, checkPackUpdates, updatePack,
  updateStatusLine, isLinked, canPublish
} from "../packActions";
import { showAlert, showConfirm } from "../dialogs";
import { invoke } from "@tauri-apps/api/core";
import { onMounted, onBeforeUnmount, ref, computed, watch } from "vue";
import { Search, Plus, RefreshCw, X, ChevronDown, ChevronRight } from 'lucide-vue-next';

const searchQuery = ref("");
const activeCategory = ref("All");
let onlineSearchDebounce: ReturnType<typeof setTimeout> | null = null;

function matchesMod(mod: any) {
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
}

const filteredMods = computed(() => {
  const sourceArray = store.currentMode === 'Online' ? store.onlineMods : store.mods;
  const filtered = sourceArray.filter(matchesMod);

  if (store.currentMode === 'Installed') {
    return filtered.sort((a: any, b: any) => a.name.localeCompare(b.name));
  }

  return filtered;
});

const COLLAPSED_KEY = "ron.collapsedSections";

interface Overrides { collapsed: string[]; expanded: string[] }

function readOverrides(): Overrides {
  const empty: Overrides = { collapsed: [], expanded: [] };
  try {
    const raw = localStorage.getItem(COLLAPSED_KEY);
    if (raw === null) return empty;
    const parsed = JSON.parse(raw);
    const strings = (v: any): string[] => Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
    if (Array.isArray(parsed)) return { collapsed: strings(parsed), expanded: [] };
    if (parsed && typeof parsed === "object") {
      return { collapsed: strings(parsed.collapsed), expanded: strings(parsed.expanded) };
    }
    return empty;
  } catch {
    return empty;
  }
}

const overrides = ref<Overrides>(readOverrides());

function writeOverrides() {
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify(overrides.value));
  } catch {
    // storage unavailable, state stays in memory
  }
}

const activePackId = computed(() => {
  const name = store.activePresetName || "";
  return name.startsWith("pack:") ? name.slice(5) : null;
});

const filterActive = computed(() =>
  searchQuery.value !== "" || activeCategory.value !== "All" || store.activeDeploymentFilter !== "All"
);

const sectionData = computed(() =>
  sectionMods(store.mods, store.packs, activePackId.value, matchesMod)
);

const hasPacks = computed(() => store.packs.length > 0);

const displaySections = computed<Section[]>(() => {
  if (store.currentMode === 'Online') {
    return [{ id: "online", title: "", version: null, isActive: false, mods: filteredMods.value, enabledCount: 0, total: filteredMods.value.length }];
  }
  const all = sectionData.value.sections;
  return filterActive.value ? all.filter((s) => s.mods.length > 0) : all;
});

function defaultCollapsed(section: Section) {
  return section.id !== LOOSE_ID && !section.isActive;
}

function isCollapsed(section: Section) {
  if (filterActive.value) return false;
  if (overrides.value.collapsed.includes(section.id)) return true;
  if (overrides.value.expanded.includes(section.id)) return false;
  return defaultCollapsed(section);
}

function toggleCollapsed(section: Section) {
  if (filterActive.value) return;
  const collapse = !isCollapsed(section);
  const others = (list: string[]) => list.filter((id) => id !== section.id);
  overrides.value = collapse
    ? { collapsed: [...others(overrides.value.collapsed), section.id], expanded: others(overrides.value.expanded) }
    : { collapsed: others(overrides.value.collapsed), expanded: [...others(overrides.value.expanded), section.id] };
  writeOverrides();
}

let activePackSeen = false;

watch(activePackId, (id) => {
  if (!activePackSeen) {
    activePackSeen = true;
    return;
  }
  if (!id) return;
  overrides.value = {
    collapsed: overrides.value.collapsed.filter((x) => x !== id),
    expanded: overrides.value.expanded.filter((x) => x !== id),
  };
  writeOverrides();
});

function sectionHeaderless(section: Section) {
  return store.currentMode === 'Online' || (section.id === LOOSE_ID && !hasPacks.value);
}

function sectionCount(section: Section) {
  if (filterActive.value) {
    return `${section.mods.length} ${section.mods.length === 1 ? "match" : "matches"}`;
  }
  return `${section.enabledCount} of ${section.total} enabled`;
}

function enabledInTitle(mod: any, section: Section) {
  if (mod.nexus_mod_id == null) return null;
  const other = sectionData.value.enabledElsewhere.get(mod.nexus_mod_id);
  return other && other.id !== section.id ? other.title : null;
}

function modsOfSection(section: Section) {
  const known = new Set(store.packs.map((p) => p.pack_id));
  return store.mods.filter((m: any) =>
    section.id === LOOSE_ID ? !m.pack || !known.has(m.pack.pack_id) : m.pack?.pack_id === section.id
  );
}

async function setSectionEnabled(section: Section, enable: boolean) {
  try {
    for (const mod of modsOfSection(section)) {
      if (mod.nexus_mod_id == null) continue;
      await invoke("toggle_mod", { modId: mod.id, enable });
    }
  } catch (e: any) {
    console.error("Section toggle failed:", e);
    await showAlert("Failed to update mods: " + e);
  }
  try {
    await rescanMods();
  } catch (e) {
    console.error("Failed to refresh mods:", e);
  }
}

function packOf(section: Section) {
  return section.id === LOOSE_ID ? undefined : store.packs.find((p) => p.pack_id === section.id);
}

async function copySectionLink(section: Section) {
  const pack = packOf(section);
  if (pack) await copyInviteLink(pack);
}

async function applySectionPack(section: Section) {
  const pack = store.packs.find((p) => p.pack_id === section.id);
  if (!pack) return;
  try {
    await applyPack(pack);
  } catch (e: any) {
    console.error("Failed to apply pack:", e);
    await showAlert("Failed to apply pack: " + e);
  }
}

async function loadMoreMods() {
  if (store.isFetchingOnline || !store.hasMoreOnlineMods) return;
  store.isFetchingOnline = true;
  try {
    console.log("[ONLINE-SEARCH] loadMore query:", searchQuery.value.trim(), "offset:", store.onlinePageOffset);
    const result: any = await invoke("search_nexus_mods", {
      offset: store.onlinePageOffset,
      searchQuery: searchQuery.value.trim() || null
    });
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

async function searchOnlineMods(reset = true) {
  if (store.currentMode !== "Online") return;
  store.isFetchingOnline = true;
  if (reset) {
    store.onlinePageOffset = 0;
    store.hasMoreOnlineMods = true;
  }

  try {
    const offset = reset ? 0 : store.onlinePageOffset;
    console.log("[ONLINE-SEARCH] searchOnlineMods query:", searchQuery.value.trim(), "offset:", offset, "reset:", reset);
    const result: any = await invoke("search_nexus_mods", {
      offset,
      searchQuery: searchQuery.value.trim() || null
    });
    const nodes = result?.data?.mods?.nodes || [];
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
        }));
    store.onlineMods = reset ? formatted : [...store.onlineMods, ...formatted];
    store.onlinePageOffset = offset + 20;
    store.hasMoreOnlineMods = nodes.length >= 20;
  } catch (e) {
    console.error("Failed to search online mods", e);
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

watch(searchQuery, () => {
  if (store.currentMode !== "Online") return;
  if (onlineSearchDebounce) clearTimeout(onlineSearchDebounce);
  onlineSearchDebounce = setTimeout(() => {
    searchOnlineMods(true);
  }, 350);
});

watch(() => store.currentMode, (mode) => {
  if (mode === "Online") {
    searchOnlineMods(true);
  }
});

onBeforeUnmount(() => {
  if (onlineSearchDebounce) clearTimeout(onlineSearchDebounce);
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
    await showAlert("Failed to toggle mod deployment: " + e);
    return;
  }
  if (store.currentMode !== 'Installed') return;
  try {
    await rescanMods();
  } catch (e: any) {
    console.error("Refresh after toggle failed:", e);
    await showAlert("Mod toggled, but the list could not be refreshed: " + e);
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
  if (!(await showConfirm(`Are you absolutely sure you want to completely remove ${mod.name}?`, { danger: true, confirmLabel: "Remove" }))) return;
  
  try {
    await invoke("delete_mod", { modId: mod.id });
  } catch (e: any) {
    console.error("Engine failed to purge mod:", e);
    await showAlert("Purge Failure: " + e);
  }
  try {
    await rescanMods();
  } catch (e) {
    console.error("Failed to refresh mods:", e);
  }
}

async function startModUpdateFlow(mod: any) {
  const modId = mod?.nexus_mod_id || Number.parseInt(mod?.id, 10);
  if (!modId || Number.isNaN(modId)) {
    await showAlert("This mod is missing a Nexus mod ID, so update flow can't be started");
    return;
  }

  const filesUrl = `https://www.nexusmods.com/${store.gameDomainStore}/mods/${modId}?tab=files`;
  try {
    await invoke("open_browser_url", { url: filesUrl });
    store.awaitingDropForId = String(modId);
    store.installingModPath = null;
  } catch (e: any) {
    console.error("Failed to start mod update flow:", e);
    await showAlert("Failed to open Nexus page: " + e);
  }
}

async function refreshMods(checkForUpdates = true) {
  try {
    await rescanMods();
    if (checkForUpdates) {
      await invoke("refresh_installed_mod_updates");
      await rescanMods();
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
  await checkPackUpdates();
}
</script>

<template>
  <main class="main-content">
    <header class="top-bar">
      <div class="search-container">
        <Search class="search-icon" :size="18" color="#94a3b8" />
        <input type="text" placeholder="Search mods..." class="search-bar" v-model="searchQuery" />
      </div>
      
      <button class="btn add-mod-btn" @click="manualPickArchive" title="Install from .zip, .7z, .rar or .pak archive">
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
        <p v-if="store.currentMode === 'Installed' && store.mods.length === 0">Drop a .zip, .7z, .rar or .pak file anywhere to install a new mod!</p>
      </div>
      
      <section
        v-for="section in displaySections"
        :key="section.id"
        :id="`section-${section.id}`"
        class="mod-section"
      >
        <div v-if="!sectionHeaderless(section)" class="section-header">
          <button class="section-toggle" @click="toggleCollapsed(section)">
            <component :is="isCollapsed(section) ? ChevronRight : ChevronDown" :size="18" />
            <span class="section-title">{{ section.title }}</span>
            <span v-if="section.version" class="section-version">{{ section.version }}</span>
          </button>
          <span v-if="section.isActive" class="section-active-tag">active</span>
          <span v-if="packOf(section)?.published?.server && packOf(section)!.published!.dirty" class="section-dirty-tag">Changes not uploaded</span>
          <span class="section-count">{{ sectionCount(section) }}</span>
          <template v-if="packOf(section) && store.packUpdates[section.id]">
            <span v-if="updateStatusLine(store.packUpdates[section.id])" class="section-update-tag">{{ updateStatusLine(store.packUpdates[section.id]) }}</span>
          </template>
          <span v-if="store.packProgress[section.id]" class="section-progress">{{ progressLine(store.packProgress[section.id]) }}</span>
          <div class="section-actions">
            <template v-if="packOf(section)">
              <button
                v-if="canPublish() && !packOf(section)!.source"
                class="section-btn"
                title="Publish modpack"
                @click="publishPack(packOf(section)!)"
                :disabled="!!store.inFlight[section.id]"
              >Publish</button>
              <button v-if="packOf(section)!.published?.server" class="section-btn" @click="copySectionLink(section)">Copy invite link</button>
              <button
                v-if="store.packUpdates[section.id]?.status === 'available'"
                class="section-btn"
                @click="updatePack(packOf(section)!)"
                :disabled="!!store.inFlight[section.id]"
              >Update</button>
            </template>
            <template v-if="section.id === LOOSE_ID">
              <button v-if="!filterActive" class="section-btn" @click="setSectionEnabled(section, true)">Enable all</button>
              <button v-if="!filterActive" class="section-btn" @click="setSectionEnabled(section, false)">Disable all</button>
            </template>
            <button v-else-if="!section.isActive" class="section-btn" @click="applySectionPack(section)">Apply</button>
            <button v-else-if="!filterActive" class="section-btn" @click="setSectionEnabled(section, false)">Disable all</button>
          </div>
        </div>

        <div v-show="sectionHeaderless(section) || !isCollapsed(section)" class="card-grid">
          <div
            v-for="mod in section.mods"
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
              v-if="store.currentMode === 'Installed' && mod.update_available && !mod.pack"
              class="update-available-banner"
              @click.stop="startModUpdateFlow(mod)"
              title="Open Nexus files page and import the updated archive"
            >
              Update Available
            </button>

            <img :src="mod.thumbnail_url || 'https://images.unsplash.com/photo-1595590424283-b8f17842773f?auto=format&fit=crop&q=80&w=400&h=400'" alt="Mod Cover" class="mod-image" loading="lazy" decoding="async" />
            <div class="mod-info">
              <div v-if="store.currentMode === 'Installed' && enabledInTitle(mod, section)" class="enabled-in">Enabled in {{ enabledInTitle(mod, section) }}</div>
              <h3>{{ mod.name }}</h3>
            </div>
            <button v-if="!mod.is_online && !(mod.pack && isLinked(mod.pack.pack_id))" class="delete-mod" @click.stop="removeMod(mod)" title="Remove Mod Completely">
              <X :size="16" color="#ffffff" />
            </button>
          </div>
        </div>
      </section>

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

.mod-section {
  grid-column: 1 / -1;
}

.card-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 1.5rem;
}

.section-header {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  margin: 0.5rem 0 1rem;
  padding-bottom: 0.5rem;
  border-bottom: 1px solid var(--border-light);
}

.section-toggle {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  background: none;
  border: none;
  color: var(--text-main);
  font-size: 1.05rem;
  font-weight: 700;
  cursor: pointer;
  padding: 0;
}

.section-version {
  color: var(--text-muted);
  font-weight: 500;
  font-size: 0.9rem;
}

.section-active-tag {
  background: rgba(34, 197, 94, 0.15);
  color: #22c55e;
  border: 1px solid #22c55e;
  border-radius: 99px;
  padding: 0.1rem 0.6rem;
  font-size: 0.75rem;
  font-weight: 700;
}

.section-dirty-tag {
  background: rgba(245, 158, 11, 0.12);
  color: #f59e0b;
  border: 1px solid rgba(245, 158, 11, 0.5);
  border-radius: 99px;
  padding: 0.1rem 0.6rem;
  font-size: 0.75rem;
  font-weight: 700;
}

.section-update-tag {
  background: rgba(59, 130, 246, 0.12);
  color: #3b82f6;
  border: 1px solid rgba(59, 130, 246, 0.5);
  border-radius: 99px;
  padding: 0.1rem 0.6rem;
  font-size: 0.75rem;
  font-weight: 700;
  white-space: nowrap;
}

.section-progress {
  color: var(--text-muted);
  font-size: 0.8rem;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.section-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.section-count {
  color: var(--text-muted);
  font-size: 0.85rem;
}

.section-actions {
  margin-left: auto;
  display: flex;
  gap: 0.5rem;
}

.section-btn {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  color: var(--text-main);
  padding: 0.35rem 0.9rem;
  border-radius: 6px;
  font-weight: 600;
  font-size: 0.85rem;
  cursor: pointer;
  transition: all 0.2s;
}

.section-btn:hover:not(:disabled) {
  border-color: var(--accent-primary);
  color: var(--accent-primary);
}

.enabled-in {
  font-size: 0.75rem;
  color: #a7f3d0;
  margin-bottom: 0.25rem;
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
