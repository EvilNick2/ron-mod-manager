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

export interface PackRef {
  pack_id: string;
  name: string;
  version: string | null;
}

export interface PackProgress {
  pack_id: string;
  stage: string;
  done: number;
  total: number;
  file: string | null;
  bytes_done: number;
  bytes_total: number;
}

export interface CloseBlocked {
  dirty: PackRef[];
  uploading: PackRef[];
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
  postInstallModId: null as string | null,

  google: { signInAvailable: false, linksAvailable: false, email: null as string | null },
  packProgress: {} as Record<string, PackProgress>,
  inFlight: {} as Record<string, true>,
  closeBlocked: null as CloseBlocked | null
});
