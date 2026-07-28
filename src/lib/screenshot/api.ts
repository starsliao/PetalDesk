import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_TOOL_SETTINGS,
  type ColorFormat,
  type PinnedScreenshotApi,
  type ScreenshotApi,
  type ScreenshotExportRequest,
  type ScreenshotExportResult,
  type ScreenshotSession,
  type ScreenshotSettings,
  type ToolSettings,
} from "./types";

interface BackendError {
  code?: string;
  message?: string;
}

interface PrepareResult {
  canceled: boolean;
  ticket?: string | null;
}

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error && "message" in error) return String(error.message);
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as BackendError;
      if (parsed.message) return parsed.message;
    } catch {
      return error;
    }
  }
  return "截图操作失败，请重试。";
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}

function binary(value: ArrayBuffer | Uint8Array | number[]): Uint8Array {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (Array.isArray(value)) return Uint8Array.from(value);
  throw new Error("截图数据格式无效。请重新截图。");
}

function normalizedSettings(value: ScreenshotSettings): ScreenshotSettings {
  return {
    schemaVersion: Number(value.schemaVersion) || 1,
    shortcut: value.shortcut || "F1",
    lastSaveDirectory: value.lastSaveDirectory ?? null,
    colorFormat: value.colorFormat === "rgb" ? "rgb" : "hex",
    toolParameters: { ...DEFAULT_TOOL_SETTINGS, ...(value.toolParameters ?? {}) },
  };
}

export const screenshotApi: ScreenshotApi = {
  async getSession(sessionId?: string): Promise<ScreenshotSession | null> {
    const session = await command<ScreenshotSession | null>("get_screenshot_session");
    if (!session) return null;
    if (sessionId && session.id !== sessionId) throw new Error("截图会话已经更新，请重新打开截图窗口。");
    return session;
  },

  async getFrame(sessionId: string): Promise<Uint8Array> {
    try {
      return binary(await invoke<ArrayBuffer | Uint8Array | number[]>("get_screenshot_frame", { sessionId }));
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },

  async present(sessionId: string): Promise<void> {
    await command<void>("present_screenshot_capture", { sessionId });
  },

  async cancel(sessionId?: string): Promise<void> {
    await command<void>("cancel_screenshot_capture", { sessionId });
  },

  async getSettings(): Promise<ScreenshotSettings> {
    return normalizedSettings(await command<ScreenshotSettings>("get_screenshot_settings"));
  },

  async setShortcut(shortcut: string): Promise<ScreenshotSettings> {
    return normalizedSettings(await command<ScreenshotSettings>("set_screenshot_shortcut", { shortcut }));
  },

  async saveToolSettings(settings: ToolSettings, colorFormat: ColorFormat): Promise<void> {
    await command<void>("update_screenshot_settings", {
      patch: { colorFormat, toolParameters: settings },
    });
  },

  async exportPng(request: ScreenshotExportRequest, png: Uint8Array): Promise<ScreenshotExportResult> {
    const prepared = await command<PrepareResult>("prepare_screenshot_export", { request });
    if (prepared.canceled || !prepared.ticket) {
      return { action: request.action, canceled: true };
    }
    try {
      return await invoke<ScreenshotExportResult>("commit_screenshot_export", png, {
        headers: { "x-petaldesk-export-token": prepared.ticket },
      });
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },
};

export const pinnedScreenshotApi: PinnedScreenshotApi = {
  async getPng(pinId: string): Promise<Uint8Array> {
    try {
      return binary(await invoke<ArrayBuffer | Uint8Array | number[]>("get_pinned_screenshot", { pinId }));
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },
  async copy(pinId: string): Promise<void> {
    await command<void>("copy_pinned_screenshot", { pinId });
  },
  async save(pinId: string): Promise<{ savedPath?: string | null; canceled?: boolean }> {
    return command("save_pinned_screenshot", { pinId });
  },
  async close(pinId: string): Promise<void> {
    await command<void>("close_pinned_screenshot", { pinId });
  },
};
