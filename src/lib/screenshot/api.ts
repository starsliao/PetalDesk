import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_TOOL_SETTINGS,
  type ColorFormat,
  type LongCaptureCapability,
  type LongCaptureStatus,
  type PinnedScreenshotApi,
  type PreparedLongCaptureAnnotationExport,
  type ScreenshotApi,
  type ScreenshotExportAction,
  type ScreenshotExportRequest,
  type ScreenshotExportResult,
  type ScreenshotSession,
  type ScreenshotSettings,
  type StartLongCaptureRequest,
  type ToolSettings,
} from "./types";
import { normalizeLongCaptureCapability, normalizeLongCaptureStatus } from "./long-capture";

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

async function longCaptureControl(name: string, jobId: string): Promise<LongCaptureStatus> {
  const status = await command<LongCaptureStatus | null>(name, { jobId });
  if (status) return normalizeLongCaptureStatus(status);
  const current = await command<LongCaptureStatus | null>("get_long_capture_status", { jobId });
  if (!current) throw new Error("长截图任务不存在或已结束。");
  return normalizeLongCaptureStatus(current);
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

  async getLongCaptureCapability(): Promise<LongCaptureCapability> {
    const value = await command<LongCaptureCapability & { supported?: boolean }>("get_long_capture_capability");
    return normalizeLongCaptureCapability(value);
  },

  async startLongCapture(request: StartLongCaptureRequest): Promise<LongCaptureStatus> {
    return normalizeLongCaptureStatus(await command<LongCaptureStatus>("start_long_capture", { request }));
  },

  async pauseLongCapture(jobId: string): Promise<LongCaptureStatus> {
    return longCaptureControl("pause_long_capture", jobId);
  },

  async resumeLongCapture(jobId: string): Promise<LongCaptureStatus> {
    return longCaptureControl("resume_long_capture", jobId);
  },

  async retryLongCapture(jobId: string): Promise<LongCaptureStatus> {
    return longCaptureControl("retry_long_capture_segment", jobId);
  },

  async undoLongCapture(jobId: string): Promise<LongCaptureStatus> {
    return longCaptureControl("undo_long_capture_segment", jobId);
  },

  async finishLongCapture(jobId: string): Promise<LongCaptureStatus> {
    return longCaptureControl("finish_long_capture", jobId);
  },

  async cancelLongCapture(jobId: string): Promise<LongCaptureStatus> {
    return longCaptureControl("cancel_long_capture", jobId);
  },

  async getLongCaptureStatus(jobId: string): Promise<LongCaptureStatus | null> {
    const status = await command<LongCaptureStatus | null>("get_long_capture_status", { jobId });
    return status ? normalizeLongCaptureStatus(status) : null;
  },

  async getLongCaptureTile(jobId: string, y: number, height: number): Promise<Uint8Array> {
    try {
      return binary(await invoke<ArrayBuffer | Uint8Array | number[]>("get_long_capture_tile", { jobId, y, height }));
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },

  async exportLongCapture(jobId: string, action: ScreenshotExportAction): Promise<ScreenshotExportResult> {
    return command<ScreenshotExportResult>("export_long_capture", { jobId, action });
  },

  async prepareLongCaptureAnnotationExport(
    jobId: string,
    action: ScreenshotExportAction,
  ): Promise<PreparedLongCaptureAnnotationExport> {
    return command<PreparedLongCaptureAnnotationExport>("prepare_long_capture_annotation_export", { jobId, action });
  },

  async uploadLongCaptureAnnotationStrip(ticket: string, y: number, png: Uint8Array): Promise<void> {
    try {
      await invoke<void>("upload_long_capture_annotation_strip", png, {
        headers: {
          "x-petaldesk-long-export-token": ticket,
          "x-petaldesk-long-export-y": String(y),
        },
      });
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },

  async finishLongCaptureAnnotationExport(ticket: string): Promise<ScreenshotExportResult> {
    return command<ScreenshotExportResult>("finish_long_capture_annotation_export", { ticket });
  },

  async cancelLongCaptureAnnotationExport(ticket: string): Promise<void> {
    await command<void>("cancel_long_capture_annotation_export", { ticket });
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
