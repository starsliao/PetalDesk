import { canvasPngBytes } from "./image";
import { renderComposite } from "./render";
import type { Annotation, Rect } from "./types";
import { createExportWorkerRequest, isExportWorkerResponse } from "./export-protocol";

export interface ExportWorkerLike {
  onmessage: ((event: MessageEvent<unknown>) => void) | null;
  onerror: ((event: ErrorEvent) => void) | null;
  postMessage(message: unknown, transfer: Transferable[]): void;
  terminate(): void;
}

export interface ScreenshotExportOptions {
  framePng?: Uint8Array | null;
  workerFactory?: (() => ExportWorkerLike) | null;
  workerTimeoutMs?: number;
  fallbackExporter?: typeof exportSelectionPngOnMainThread;
}

function workerSupported(): boolean {
  return typeof Worker !== "undefined"
    && typeof OffscreenCanvas !== "undefined"
    && typeof createImageBitmap === "function";
}

function defaultWorkerFactory(): ExportWorkerLike {
  return new Worker(new URL("./export.worker.ts", import.meta.url), { type: "module", name: "petaldesk-screenshot-export" });
}

function requestId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `export-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

async function exportWithWorker(
  framePng: Uint8Array,
  frameWidth: number,
  frameHeight: number,
  annotations: readonly Annotation[],
  selection: Rect,
  factory: () => ExportWorkerLike,
  timeoutMs: number,
): Promise<Uint8Array> {
  const worker = factory();
  const id = requestId();
  const request = createExportWorkerRequest(id, framePng, frameWidth, frameHeight, annotations, selection);
  return new Promise<Uint8Array>((resolve, reject) => {
    const timeout = setTimeout(() => {
      worker.terminate();
      reject(new Error("离屏截图合成超时。"));
    }, timeoutMs);
    const finish = (): void => {
      clearTimeout(timeout);
      worker.terminate();
    };
    worker.onerror = (event) => {
      finish();
      reject(new Error(event.message || "离屏截图合成失败。"));
    };
    worker.onmessage = (event) => {
      if (!isExportWorkerResponse(event.data, id)) return;
      finish();
      if (event.data.type === "error") reject(new Error(event.data.message));
      else resolve(new Uint8Array(event.data.png));
    };
    worker.postMessage(request, [request.framePng]);
  });
}

export async function exportSelectionPng(
  source: CanvasImageSource,
  frameWidth: number,
  frameHeight: number,
  annotations: readonly Annotation[],
  selection: Rect,
  options: ScreenshotExportOptions = {},
): Promise<Uint8Array> {
  const fallback = options.fallbackExporter ?? exportSelectionPngOnMainThread;
  const factory = options.workerFactory === undefined
    ? (workerSupported() ? defaultWorkerFactory : null)
    : options.workerFactory;
  if (options.framePng && factory) {
    try {
      return await exportWithWorker(
        options.framePng,
        frameWidth,
        frameHeight,
        annotations,
        selection,
        factory,
        options.workerTimeoutMs ?? 30_000,
      );
    } catch {
      // Older WebView2 builds can expose OffscreenCanvas but fail during PNG conversion.
    }
  }
  return fallback(source, frameWidth, frameHeight, annotations, selection);
}

export async function exportSelectionPngOnMainThread(
  source: CanvasImageSource,
  frameWidth: number,
  frameHeight: number,
  annotations: readonly Annotation[],
  selection: Rect,
): Promise<Uint8Array> {
  const full = document.createElement("canvas");
  full.width = frameWidth;
  full.height = frameHeight;
  const fullContext = full.getContext("2d", { willReadFrequently: true });
  if (!fullContext) throw new Error("当前 WebView2 不支持截图导出。");
  renderComposite(fullContext, source, frameWidth, frameHeight, annotations);

  const output = document.createElement("canvas");
  output.width = Math.max(1, Math.round(selection.width));
  output.height = Math.max(1, Math.round(selection.height));
  const outputContext = output.getContext("2d");
  if (!outputContext) throw new Error("当前 WebView2 不支持截图导出。");
  outputContext.drawImage(
    full,
    Math.round(selection.x),
    Math.round(selection.y),
    output.width,
    output.height,
    0,
    0,
    output.width,
    output.height,
  );
  return canvasPngBytes(output);
}
