import { reactive, watch } from 'vue';

export interface PackManifest {
  pack_id: string;
  name: string;
  version: string | null;
  mod_ids: string[];
  imported_at: number;
  source?: {
    kind: string;
    server?: string;
    folder_id?: string;
    resource_key?: string;
    invite?: string;
    uploader?: string;
  };
  installed_hashes?: Record<string, { hash: string; files: Record<string, string> }>;
  published?: { server: string | null; dirty: boolean };
}

export interface ServerUser {
  id: string;
  name: string;
  role: string | null;
}

export interface ServerStatus {
  base: string;
  signed_in: boolean;
  user: ServerUser | null;
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
  overall_done: number;
  overall_total: number;
  wait_secs: number;
}

export interface ModRef {
  key: string;
  nexus_mod_id: number | null;
  name: string;
  version: string | null;
}

export interface UpdateCheck {
  pack_id: string;
  status: "current" | "available" | "error" | "unsupported";
  installed_version: string | null;
  remote_version: string | null;
  added: ModRef[];
  changed: ModRef[];
  removed: ModRef[];
  download_bytes: number;
  error: string | null;
}

export interface CloseBlocked {
  dirty: PackRef[];
  uploading: PackRef[];
  downloading: PackRef[];
}

export interface DialogRequest {
  id: number;
  kind: "alert" | "confirm" | "prompt" | "copy" | "choice";
  title: string;
  message: string;
  value: string;
  confirmLabel: string;
  cancelLabel: string;
  danger: boolean;
  validate?: (v: string) => string | null;
  choices?: { label: string; value: string }[];
  resolve: (result: boolean | string | null) => void;
}

export interface PackChangelogRequest {
  packId: string;
  mode: "history" | "update";
  // The size and counts line shown above the list in update mode
  summary: string;
  resolve: (update: boolean) => void;
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
  selectedPack: null as string | null,
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

  servers: [] as ServerStatus[],
  packAccessFor: null as string | null,
  serverAdminFor: null as string | null,
  packProgress: {} as Record<string, PackProgress>,
  inFlight: {} as Record<string, true>,
  opNames: {} as Record<string, string>,
  progressHidden: {} as Record<string, true>,
  cancelling: {} as Record<string, true>,
  dialogs: [] as DialogRequest[],
  packUpdates: {} as Record<string, UpdateCheck>,
  packChangelog: null as PackChangelogRequest | null,
  closeBlocked: null as CloseBlocked | null
});

// The right panel shows one thing at a time, a mod or a pack
watch(() => store.selectedPack, (id) => {
  if (id) store.selectedMod = null;
}, { flush: "sync" });
watch(() => store.selectedMod, (mod) => {
  if (mod) store.selectedPack = null;
}, { flush: "sync" });
