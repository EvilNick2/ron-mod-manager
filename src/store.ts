import { reactive } from 'vue';

export interface PackManifest {
  pack_id: string;
  name: string;
  version: string | null;
  mod_ids: string[];
  imported_at: number;
  source?: { kind: string; folder_id: string; resource_key: string | null };
  installed_hashes?: Record<string, { hash: string; files: Record<string, string> }>;
  published?: { folder_id: string; dirty: boolean };
}

export const store = reactive({
  apiKeyStore: "",
  currentMode: "Installed",
  gamePathStore: "",
  gameAppIdStore: 1144200,
  gameDomainStore: "readyornot",
  launchOptionsStore: "",
  modStoragePathStore: "",
  defaultModStoragePath: "",
  isSettingsOpen: false,
  isBlurEnabled: true,
  activeDeploymentFilter: "All" as "All" | "Enabled" | "Disabled",
  activePresetName: "" as string,
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
  packs: [] as PackManifest[],
  installingModPath: null as string | null,
  awaitingDropForId: null as string | null,
  postInstallModId: null as string | null
});
