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
  presets: {} as Record<string, string[]>,
  installingModPath: null as string | null,
  awaitingDropForId: null as string | null,
  postInstallModId: null as string | null
});
