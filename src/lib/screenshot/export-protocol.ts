import type { Annotation, Rect } from "./types";

export const SCREENSHOT_EXPORT_PROTOCOL_VERSION = 1 as const;

export interface ScreenshotExportWorkerRequest {
  type: "export";
  version: typeof SCREENSHOT_EXPORT_PROTOCOL_VERSION;
  requestId: string;
  framePng: ArrayBuffer;
  frameWidth: number;
  frameHeight: number;
  annotations: Annotation[];
  selection: Rect;
}

export interface ScreenshotExportWorkerResult {
  type: "result";
  version: typeof SCREENSHOT_EXPORT_PROTOCOL_VERSION;
  requestId: string;
  png: ArrayBuffer;
}

export interface ScreenshotExportWorkerError {
  type: "error";
  version: typeof SCREENSHOT_EXPORT_PROTOCOL_VERSION;
  requestId: string;
  message: string;
}

export type ScreenshotExportWorkerResponse = ScreenshotExportWorkerResult | ScreenshotExportWorkerError;

export function createExportWorkerRequest(
  requestId: string,
  framePng: Uint8Array,
  frameWidth: number,
  frameHeight: number,
  annotations: readonly Annotation[],
  selection: Rect,
): ScreenshotExportWorkerRequest {
  const copiedPng = framePng.slice();
  return {
    type: "export",
    version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
    requestId,
    framePng: copiedPng.buffer,
    frameWidth,
    frameHeight,
    // Svelte state is proxy-backed and cannot be sent through structuredClone directly.
    annotations: JSON.parse(JSON.stringify(annotations)) as Annotation[],
    selection: { ...selection },
  };
}

export function isExportWorkerResponse(value: unknown, requestId: string): value is ScreenshotExportWorkerResponse {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<ScreenshotExportWorkerResponse>;
  return candidate.version === SCREENSHOT_EXPORT_PROTOCOL_VERSION
    && candidate.requestId === requestId
    && (candidate.type === "result" || candidate.type === "error");
}
