import { describe, expect, it, vi } from "vitest";
import { exportSelectionPng, type ExportWorkerLike } from "./export";
import {
  SCREENSHOT_EXPORT_PROTOCOL_VERSION,
  createExportWorkerRequest,
  isExportWorkerResponse,
  type ScreenshotExportWorkerRequest,
} from "./export-protocol";
import type { Annotation } from "./types";

const selection = { x: 10, y: 20, width: 100, height: 80 };
const annotations: Annotation[] = [{
  id: "line-1",
  kind: "line",
  line: "arrow",
  from: { x: 20, y: 30 },
  to: { x: 80, y: 70 },
  stroke: { color: "#f00", width: 3, lineStyle: "solid" },
}];

class ResultWorker implements ExportWorkerLike {
  onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  terminate = vi.fn();
  posted: ScreenshotExportWorkerRequest | null = null;
  transfers: Transferable[] = [];

  postMessage(message: unknown, transfer: Transferable[]): void {
    this.posted = message as ScreenshotExportWorkerRequest;
    this.transfers = transfer;
    const png = Uint8Array.from([137, 80, 78, 71]).buffer;
    queueMicrotask(() => this.onmessage?.({ data: {
      type: "result",
      version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
      requestId: this.posted!.requestId,
      png,
    } } as MessageEvent));
  }
}

describe("screenshot export worker protocol", () => {
  it("copies transferable frame bytes and structured annotation data", () => {
    const frame = Uint8Array.from([1, 2, 3, 4]);
    const proxiedAnnotations = new Proxy(annotations, {});
    const request = createExportWorkerRequest("request-1", frame, 800, 600, proxiedAnnotations, selection);
    expect(request).toMatchObject({
      type: "export",
      version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
      requestId: "request-1",
      frameWidth: 800,
      frameHeight: 600,
      selection,
    });
    expect(new Uint8Array(request.framePng)).toEqual(frame);
    expect(request.framePng).not.toBe(frame.buffer);
    request.annotations[0].id = "changed";
    expect(annotations[0].id).toBe("line-1");
  });

  it("accepts only matching protocol responses", () => {
    expect(isExportWorkerResponse({
      type: "result",
      version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
      requestId: "request-1",
      png: new ArrayBuffer(0),
    }, "request-1")).toBe(true);
    expect(isExportWorkerResponse({
      type: "result",
      version: 99,
      requestId: "request-1",
    }, "request-1")).toBe(false);
    expect(isExportWorkerResponse({
      type: "result",
      version: SCREENSHOT_EXPORT_PROTOCOL_VERSION,
      requestId: "other",
    }, "request-1")).toBe(false);
  });

  it("uses the lazy worker and transfers only the copied PNG buffer", async () => {
    const worker = new ResultWorker();
    const fallback = vi.fn();
    const result = await exportSelectionPng(
      {} as CanvasImageSource,
      800,
      600,
      annotations,
      selection,
      {
        framePng: Uint8Array.from([1, 2, 3]),
        workerFactory: () => worker,
        fallbackExporter: fallback,
      },
    );
    expect(result).toEqual(Uint8Array.from([137, 80, 78, 71]));
    expect(worker.posted).toMatchObject({ type: "export", frameWidth: 800, frameHeight: 600 });
    expect(worker.transfers).toEqual([worker.posted!.framePng]);
    expect(worker.terminate).toHaveBeenCalledOnce();
    expect(fallback).not.toHaveBeenCalled();
  });

  it("falls back when workers are unsupported or fail during composition", async () => {
    const fallbackBytes = Uint8Array.from([9, 8, 7]);
    const fallback = vi.fn().mockResolvedValue(fallbackBytes);
    await expect(exportSelectionPng(
      {} as CanvasImageSource,
      800,
      600,
      annotations,
      selection,
      { framePng: Uint8Array.from([1]), workerFactory: null, fallbackExporter: fallback },
    )).resolves.toEqual(fallbackBytes);
    expect(fallback).toHaveBeenCalledOnce();

    const failedWorker: ExportWorkerLike = {
      onmessage: null,
      onerror: null,
      terminate: vi.fn(),
      postMessage() {
        queueMicrotask(() => this.onerror?.({ message: "OffscreenCanvas failed" } as ErrorEvent));
      },
    };
    fallback.mockClear();
    await expect(exportSelectionPng(
      {} as CanvasImageSource,
      800,
      600,
      annotations,
      selection,
      { framePng: Uint8Array.from([1]), workerFactory: () => failedWorker, fallbackExporter: fallback },
    )).resolves.toEqual(fallbackBytes);
    expect(fallback).toHaveBeenCalledOnce();
  });
});
