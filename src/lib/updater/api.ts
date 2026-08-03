import { invoke } from "@tauri-apps/api/core";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "ready"
  | "installing"
  | "error";

export interface UpdateSettings {
  autoUpdate: boolean;
}

export interface UpdateState {
  phase: UpdatePhase;
  currentVersion: string;
  availableVersion: string | null;
  releaseNotes: string | null;
  publishedAt: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
}

export interface UpdateInstallPreparation {
  requestId: string;
}

export interface UpdateApi {
  isSupported(): boolean;
  getSettings(): Promise<UpdateSettings>;
  setSettings(settings: UpdateSettings): Promise<UpdateSettings>;
  getState(): Promise<UpdateState>;
  check(): Promise<UpdateState>;
  download(): Promise<UpdateState>;
  installAndRestart(): Promise<void>;
  postpone(): Promise<UpdateState>;
  registerInstallWindow(): Promise<void>;
  unregisterInstallWindow(): Promise<void>;
  acknowledgeInstall(
    requestId: string,
    windowLabel: string,
    ok: boolean,
    error?: string,
  ): Promise<void>;
  listen(onState: (state: UpdateState) => void): Promise<() => void>;
}

interface BackendError {
  message?: string;
}

type RawUpdateState = Omit<Partial<UpdateState>, "phase"> & {
  phase?: string;
  status?: string;
  current_version?: string;
  available_version?: string | null;
  release_notes?: string | string[] | null;
  notes?: string | string[] | null;
  published_at?: string | null;
  downloaded_bytes?: number;
  total_bytes?: number | null;
  contentLength?: number | null;
  downloaded?: number;
};

const phaseAliases: Record<string, UpdatePhase> = {
  idle: "idle",
  checking: "checking",
  upToDate: "upToDate",
  up_to_date: "upToDate",
  "up-to-date": "upToDate",
  available: "available",
  update_available: "available",
  downloading: "downloading",
  ready: "ready",
  downloaded: "ready",
  installing: "installing",
  error: "error",
  failed: "error",
};

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function isWindowsPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const candidate = (navigator as Navigator & { userAgentData?: { platform?: string } })
    .userAgentData?.platform || navigator.platform || navigator.userAgent;
  return /win/i.test(candidate);
}

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error && "message" in error) {
    return String((error as BackendError).message);
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as BackendError;
      if (parsed.message) return parsed.message;
    } catch {
      return error;
    }
  }
  return "更新操作失败，请稍后重试。";
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}

function finiteBytes(value: unknown): number {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : 0;
}

function nullableBytes(value: unknown): number | null {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : null;
}

function releaseNotes(value: unknown): string | null {
  if (Array.isArray(value)) {
    const result = value.map(String).filter(Boolean).join("\n");
    return result || null;
  }
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

export function normalizeUpdateState(value: RawUpdateState | null | undefined): UpdateState {
  const rawPhase = String(value?.phase ?? value?.status ?? "idle");
  const phase = phaseAliases[rawPhase] ?? "idle";
  return {
    phase,
    currentVersion: String(value?.currentVersion ?? value?.current_version ?? ""),
    availableVersion: value?.availableVersion ?? value?.available_version ?? null,
    releaseNotes: releaseNotes(value?.releaseNotes ?? value?.release_notes ?? value?.notes),
    publishedAt: value?.publishedAt ?? value?.published_at ?? null,
    downloadedBytes: finiteBytes(value?.downloadedBytes ?? value?.downloaded_bytes ?? value?.downloaded),
    totalBytes: nullableBytes(value?.totalBytes ?? value?.total_bytes ?? value?.contentLength),
    error: typeof value?.error === "string" && value.error.trim() ? value.error : null,
  };
}

function normalizeSettings(value: Partial<UpdateSettings> | null | undefined): UpdateSettings {
  return { autoUpdate: value?.autoUpdate !== false };
}

export const updaterApi: UpdateApi = {
  isSupported(): boolean {
    return isTauriRuntime() && isWindowsPlatform();
  },

  async getSettings(): Promise<UpdateSettings> {
    return normalizeSettings(await command<UpdateSettings>("get_update_settings"));
  },

  async setSettings(settings): Promise<UpdateSettings> {
    return normalizeSettings(await command<UpdateSettings>("set_update_settings", { settings }));
  },

  async getState(): Promise<UpdateState> {
    return normalizeUpdateState(await command<RawUpdateState>("get_update_state"));
  },

  async check(): Promise<UpdateState> {
    return normalizeUpdateState(await command<RawUpdateState>("check_for_updates"));
  },

  async download(): Promise<UpdateState> {
    return normalizeUpdateState(await command<RawUpdateState>("download_update"));
  },

  async installAndRestart(): Promise<void> {
    await command<void>("install_update_and_restart");
  },

  async postpone(): Promise<UpdateState> {
    return normalizeUpdateState(await command<RawUpdateState>("postpone_update"));
  },

  async registerInstallWindow(): Promise<void> {
    await command<void>("register_update_install_window");
  },

  async unregisterInstallWindow(): Promise<void> {
    await command<void>("unregister_update_install_window");
  },

  async acknowledgeInstall(requestId, windowLabel, ok, error): Promise<void> {
    await command<void>("acknowledge_update_install", {
      requestId,
      windowLabel,
      ok,
      error: error || null,
    });
  },

  async listen(onState): Promise<() => void> {
    if (!isTauriRuntime()) return () => undefined;
    const { listen } = await import("@tauri-apps/api/event");
    return listen<RawUpdateState>("updater_state_changed", ({ payload }) => {
      onState(normalizeUpdateState(payload));
    });
  },
};
