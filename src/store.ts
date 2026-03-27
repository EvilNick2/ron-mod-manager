import { reactive } from 'vue';

export const store = reactive({
  apiKeyStore: "",
  currentMode: "Installed",
  gamePathStore: "",
  modStoragePathStore: "",
  defaultModStoragePath: "",
  isSettingsOpen: false,
  isBlurEnabled: true,
  activeDeploymentFilter: "All" as "All" | "Enabled" | "Disabled",
  selectedMod: null as any,
  mods: [] as any[],
  onlineMods: [] as any[],
  isFetchingOnline: false,
  onlinePageOffset: 0,
  hasMoreOnlineMods: true,
  isUpdateModalOpen: false,
  updateInfo: null as null | {
    currentVersion?: string;
    version?: string;
    date?: string;
    body?: string;
  },
  isUpdatingApp: false,
  updateProgress: null as null | number,
  updateError: "" as string,

  presets: {} as Record<string, string[]>,
  installingModPath: null as string | null,
  awaitingDropForId: null as string | null,
  postInstallModId: null as string | null
});
