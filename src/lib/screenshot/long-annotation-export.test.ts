import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Annotation, ScreenshotApi } from "./types";

const raster = vi.hoisted(() => ({
  decodePng: vi.fn(),
  canvasPngBytes: vi.fn(),
  renderComposite: vi.fn(),
}));
vi.mock("./image", () => ({ decodePng: raster.decodePng, canvasPngBytes: raster.canvasPngBytes }));
vi.mock("./render", () => ({ renderComposite: raster.renderComposite }));

import {
  annotationsForLongCaptureSource,
  exportAnnotatedLongCapture,
  planLongCaptureAnnotationStrips,
} from "./long-annotation-export";

const shape = (id: string, y: number, height: number): Annotation => ({
  id,
  kind: "shape",
  shape: "rectangle",
  rect: { x: 20, y, width: 160, height },
  stroke: { color: "#ff3b30", width: 4, lineStyle: "solid" },
  fill: null,
});

beforeEach(() => {
  raster.decodePng.mockReset().mockResolvedValue({ width: 500, height: 1152, close: vi.fn() });
  raster.canvasPngBytes.mockReset().mockResolvedValue(Uint8Array.from([137, 80, 78, 71]));
  raster.renderComposite.mockReset();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(() => ({
    drawImage: vi.fn(),
  }) as unknown as CanvasRenderingContext2D);
});

afterEach(() => vi.restoreAllMocks());

describe("bounded annotated long screenshot export", () => {
  it("adds at most 128px halo around backend-sized core strips", () => {
    expect(planLongCaptureAnnotationStrips(2500, 1024)).toEqual([
      { y: 0, height: 1024, sourceY: 0, sourceHeight: 1152, cropY: 0 },
      { y: 1024, height: 1024, sourceY: 896, sourceHeight: 1280, cropY: 128 },
      { y: 2048, height: 452, sourceY: 1920, sourceHeight: 580, cropY: 128 },
    ]);
  });

  it("filters and translates annotations into a strip source coordinate system", () => {
    const translated = annotationsForLongCaptureSource([
      shape("inside", 900, 300),
      shape("outside", 3000, 100),
    ], 896, 1280);
    expect(translated).toHaveLength(1);
    expect(translated[0]).toMatchObject({ id: "inside", rect: { y: 4, height: 300 } });
  });

  it("cancels the backend ticket when a raw strip upload fails", async () => {
    const api = {
      prepareLongCaptureAnnotationExport: vi.fn().mockResolvedValue({
        canceled: false,
        ticket: "ticket-1",
        stripHeight: 1024,
      }),
      getLongCaptureTile: vi.fn().mockResolvedValue(Uint8Array.from([1, 2, 3])),
      uploadLongCaptureAnnotationStrip: vi.fn().mockRejectedValue(new Error("upload failed")),
      finishLongCaptureAnnotationExport: vi.fn(),
      cancelLongCaptureAnnotationExport: vi.fn().mockResolvedValue(undefined),
    } as unknown as ScreenshotApi;

    await expect(exportAnnotatedLongCapture(
      api,
      "long-1",
      "copy",
      500,
      1600,
      [shape("inside", 100, 200)],
    )).rejects.toThrow("upload failed");
    expect(api.cancelLongCaptureAnnotationExport).toHaveBeenCalledWith("ticket-1");
    expect(api.finishLongCaptureAnnotationExport).not.toHaveBeenCalled();
  });
});
