<script setup lang="ts">
import { onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { store } from "./store";

import Sidebar from "./components/Sidebar.vue";
import ModGrid from "./components/ModGrid.vue";
import RightPanel from "./components/RightPanel.vue";
import SettingsModal from "./components/SettingsModal.vue";
import InstallModal from "./components/InstallModal.vue";
import UpdateModal from "./components/UpdateModal.vue";

const isDragging = ref(false);

onMounted(async () => {
  try {
    const config: any = await invoke("load_config");
    
    if (!config.game_path) {
      try {
        const detectedPath: string | null = await invoke("auto_detect_game_path");
        if (detectedPath) {
          config.game_path = detectedPath;
          await invoke("save_config", { config });
        }
      } catch (e) {
        console.warn("Silent Auto-Detect Boot Failed:", e);
      }
    }

    if (config.game_path) {
      store.gamePathStore = config.game_path;
    }

    if (config.game_app_id) {
      store.gameAppIdStore = config.game_app_id;
    }

    if (config.game_domain) {
      store.gameDomainStore = config.game_domain;
    }

    if (config.launch_options) {
      store.launchOptionsStore = config.launch_options;
    }
    
    if (config.enable_blur !== undefined && config.enable_blur !== null) {
      store.isBlurEnabled = config.enable_blur;
    }
    
    const authKey: string | null = await invoke("get_api_key");
    if (authKey) {
      store.apiKeyStore = authKey;
    }

    if (config.active_preset) {
      store.activePresetName = config.active_preset;
    }
  } catch (error) {
    console.error("Failed to load config on startup:", error);
  }

  try {
    const update = await invoke("check_for_app_update");
    if (update) {
      store.updateInfo = update as any;
      store.isUpdateModalOpen = true;
      store.updateError = "";
      store.updateProgress = null;
    }
  } catch (e) {
    console.error("Update check failed:", e);
  }


  listen("tauri://drag-enter", () => (isDragging.value = true));
  listen("tauri://drag-leave", () => (isDragging.value = false));
  
  listen("tauri://drag-drop", (e: any) => {
    isDragging.value = false;
    const paths = e.payload?.paths || e.payload;
    if (paths && paths.length > 0) {
      const file = paths[0];
      const lowerFile = file.toLowerCase();
      if (lowerFile.endsWith(".zip") || lowerFile.endsWith(".pak") || lowerFile.endsWith(".rar")) {
        store.installingModPath = file;
      } else {
        console.warn("Unsupported drop file.");
      }
    }
  });

  window.addEventListener("dragleave", () => (isDragging.value = false));
  window.addEventListener("drop", () => (isDragging.value = false));
  window.addEventListener("dragover", (e) => e.preventDefault());
});
</script>

<template>
  <div v-if="isDragging" class="drag-overlay">
    <h2>Drop Mod Archive Here</h2>
  </div>

  <div class="app-layout" :class="{ 'modal-open': store.isSettingsOpen || store.installingModPath || store.postInstallModId || store.isUpdateModalOpen, 'blur-enabled': store.isBlurEnabled }">
    <Sidebar />
    <ModGrid />
    <RightPanel />
    
    <transition name="fade">
      <SettingsModal v-if="store.isSettingsOpen" />
    </transition>

    <InstallModal v-if="store.installingModPath || store.awaitingDropForId || store.postInstallModId" />

    <transition name="fade">
      <UpdateModal v-if="store.isUpdateModalOpen" />
    </transition>
  </div>
</template>

<style>
:root {
  --bg-darker: #050508;
  --bg-dark: #0e0f14;
  --bg-card: #16181f;
  --bg-hover: #20232d;
  
  --accent-primary: #e61d36;
  --accent-hover: #ff2a46;
  
  --text-main: #ffffff;
  --text-muted: #a1a1aa;
  --text-dark: #ffffff;
  
  --border-light: #272a33;
  --border-dark: #101217;
  
  font-family: 'Inter', -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
}

body {
  margin: 0;
  padding: 0;
  overflow: hidden;
  background-color: var(--bg-darker);
  color: var(--text-main);
  user-select: none;
}

.app-layout {
  display: grid;
  grid-template-columns: 240px minmax(0, 1fr) 280px;
  height: 100vh;
  width: 100vw;
  position: relative;
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.app-layout.modal-open.blur-enabled > .sidebar,
.app-layout.modal-open.blur-enabled > .main-content,
.app-layout.modal-open.blur-enabled > .right-panel {
  filter: blur(8px) brightness(0.6);
  pointer-events: none;
}

.app-layout.modal-open:not(.blur-enabled) > .sidebar,
.app-layout.modal-open:not(.blur-enabled) > .main-content,
.app-layout.modal-open:not(.blur-enabled) > .right-panel {
  filter: brightness(0.6);
  pointer-events: none;
}

.app-layout > .sidebar,
.app-layout > .main-content,
.app-layout > .right-panel {
  transition: filter 0.3s ease, brightness 0.3s ease;
}

.drag-overlay {
  position: fixed;
  top: 0; left: 0; right: 0; bottom: 0;
  background: rgba(230, 29, 54, 0.2);
  border: 4px dashed var(--accent-primary);
  z-index: 5000;
  display: flex;
  justify-content: center;
  align-items: center;
  pointer-events: none;
}
.drag-overlay h2 {
  color: var(--accent-primary);
  font-size: 2.5rem;
  font-weight: 800;
  text-shadow: 0 4px 10px rgba(0,0,0,0.8);
}
</style>