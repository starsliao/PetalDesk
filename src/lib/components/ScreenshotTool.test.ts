import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_TOOL_SETTINGS, type ScreenshotApi } from "../screenshot";
import ScreenshotTool from "./ScreenshotTool.svelte";

class TestImageBitmap {
  width = 800;
  height = 600;
  close = vi.fn();
}

function canvasContext(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  return {
    canvas,
    save: vi.fn(),
    restore: vi.fn(),
    setTransform: vi.fn(),
    clearRect: vi.fn(),
    drawImage: vi.fn(),
    getImageData: vi.fn(() => ({ data: Uint8ClampedArray.from([18, 52, 86, 255]) })),
    beginPath: vi.fn(),
    closePath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    stroke: vi.fn(),
    fill: vi.fn(),
    fillRect: vi.fn(),
    strokeRect: vi.fn(),
    rect: vi.fn(),
    arc: vi.fn(),
    ellipse: vi.fn(),
    setLineDash: vi.fn(),
    measureText: vi.fn((value: string) => ({ width: value.length * 10 })),
    fillText: vi.fn(),
    translate: vi.fn(),
    scale: vi.fn(),
    rotate: vi.fn(),
    clip: vi.fn(),
    imageSmoothingEnabled: true,
    globalAlpha: 1,
    globalCompositeOperation: "source-over",
    lineCap: "butt",
    lineJoin: "miter",
    lineWidth: 1,
    strokeStyle: "#000",
    fillStyle: "#000",
    filter: "none",
    font: "10px sans-serif",
    textBaseline: "alphabetic",
  } as unknown as CanvasRenderingContext2D;
}

function mockApi(): ScreenshotApi & Record<"exportPng" | "saveToolSettings", ReturnType<typeof vi.fn>> {
  return {
    getSession: vi.fn().mockResolvedValue({
      id: "session-1",
      monitor: { x: -800, y: 0, width: 800, height: 600, scaleFactor: 1.5 },
      frameWidth: 800,
      frameHeight: 600,
      capturedAt: "2026-07-27T01:02:03Z",
    }),
    getFrame: vi.fn().mockResolvedValue(Uint8Array.from([137, 80, 78, 71])),
    cancel: vi.fn().mockResolvedValue(undefined),
    getSettings: vi.fn().mockResolvedValue({
      schemaVersion: 1,
      shortcut: "F1",
      lastSaveDirectory: null,
      colorFormat: "hex",
      toolParameters: { ...DEFAULT_TOOL_SETTINGS },
    }),
    setShortcut: vi.fn(),
    saveToolSettings: vi.fn().mockResolvedValue(undefined),
    exportPng: vi.fn().mockResolvedValue({ action: "copy" }),
  };
}

beforeEach(() => {
  vi.stubGlobal("ImageBitmap", TestImageBitmap);
  vi.stubGlobal("createImageBitmap", vi.fn().mockResolvedValue(new TestImageBitmap()));
  vi.stubGlobal("ResizeObserver", class {
    observe = vi.fn();
    disconnect = vi.fn();
    unobserve = vi.fn();
  });
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => window.setTimeout(() => callback(0), 0));
  vi.stubGlobal("cancelAnimationFrame", (handle: number) => window.clearTimeout(handle));
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(function (this: HTMLCanvasElement) {
    return canvasContext(this);
  });
  vi.spyOn(HTMLCanvasElement.prototype, "toBlob").mockImplementation(function (callback) {
    callback(new Blob([Uint8Array.from([137, 80, 78, 71])], { type: "image/png" }));
  });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(() => ({
    x: 0, y: 0, left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600, toJSON: () => ({}),
  }));
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 800 });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 600 });
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", { configurable: true, get: () => 650 });
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, get: () => 94 });
  Object.defineProperty(HTMLElement.prototype, "setPointerCapture", { configurable: true, value: vi.fn() });
  Object.defineProperty(HTMLElement.prototype, "releasePointerCapture", { configurable: true, value: vi.fn() });
  Object.defineProperty(HTMLElement.prototype, "hasPointerCapture", { configurable: true, value: vi.fn(() => true) });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ScreenshotTool", () => {
  it("keeps the selection resizable after annotation without changing annotation history", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(rendered.getByLabelText("截图画面")).toBeInTheDocument());
    await waitFor(() => expect(api.getFrame).toHaveBeenCalledWith("session-1"));

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 600, clientY: 450 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 200, clientY: 150 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 200, clientY: 150 });
    expect(rendered.getByText("400 × 300 px")).toBeInTheDocument();
    expect(rendered.container.querySelectorAll(".resize-handle")).toHaveLength(8);

    await fireEvent.click(rendered.getByRole("button", { name: "形状" }));
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 2, clientX: 250, clientY: 200 });
    await fireEvent.pointerMove(stage, { pointerId: 2, clientX: 420, clientY: 320 });
    await fireEvent.pointerUp(stage, { pointerId: 2, clientX: 420, clientY: 320 });

    expect(rendered.getByRole("button", { name: "撤销" })).not.toBeDisabled();
    expect(rendered.container.querySelectorAll(".resize-handle")).toHaveLength(8);
    expect(rendered.container.querySelectorAll(".annotation-handle")).toHaveLength(8);

    const southeastHandle = rendered.container.querySelector<SVGRectElement>(".resize-handle.handle-se");
    expect(southeastHandle).not.toBeNull();
    await fireEvent.pointerDown(southeastHandle!, { button: 0, pointerId: 3, clientX: 600, clientY: 450 });
    await fireEvent.pointerMove(stage, { pointerId: 3, clientX: 700, clientY: 520 });
    await fireEvent.pointerUp(stage, { pointerId: 3, clientX: 700, clientY: 520 });

    expect(rendered.getByText("500 × 370 px")).toBeInTheDocument();
    expect(rendered.container.querySelector(".selection-border")).toHaveAttribute("width", "500");
    expect(rendered.container.querySelector(".selection-border")).toHaveAttribute("height", "370");
    expect(rendered.container.querySelectorAll(".annotation-handle")).toHaveLength(8);
    expect(rendered.getByRole("button", { name: "撤销" })).not.toBeDisabled();

    await fireEvent.click(rendered.getByRole("button", { name: "撤销" }));
    expect(rendered.getByRole("button", { name: "撤销" })).toBeDisabled();
    expect(rendered.getByRole("button", { name: "重做" })).not.toBeDisabled();
  });

  it("leaves a permanent frame read in a retryable error state and closes the pending session", async () => {
    const api = mockApi();
    api.getFrame = vi.fn(() => new Promise<Uint8Array>(() => {}));
    const rendered = render(ScreenshotTool, { api, loadTimeoutMs: 30 });

    await waitFor(() => expect(rendered.getByText("无法开始截图")).toBeInTheDocument());
    expect(rendered.getByText("读取截图画面超时，请重试截图。")).toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "重试" })).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "关闭" }));
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("session-1"));
  });

  it("allows Escape to cancel while the screenshot frame is still loading", async () => {
    const api = mockApi();
    api.getFrame = vi.fn(() => new Promise<Uint8Array>(() => {}));
    render(ScreenshotTool, { api, loadTimeoutMs: 100 });

    await waitFor(() => expect(api.getFrame).toHaveBeenCalledWith("session-1"));
    await fireEvent.keyDown(window, { key: "Escape" });

    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("session-1"));
  });

  it("exports through the injected binary API and keeps a canceled save editable", async () => {
    const api = mockApi();
    api.exportPng.mockResolvedValueOnce({ action: "save", canceled: true });
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "保存截图" }));

    await waitFor(() => expect(api.exportPng).toHaveBeenCalledWith(
      { sessionId: "session-1", action: "save" },
      expect.any(Uint8Array),
    ));
    expect(rendered.getByRole("button", { name: "保存截图" })).toBeEnabled();
  });

  it("copies on a real double-click without changing the selection or annotations", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 2, clientX: 700, clientY: 500 });
    await fireEvent.pointerUp(stage, { button: 0, pointerId: 2, clientX: 700, clientY: 500 });
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 3, clientX: 700, clientY: 500 });
    await fireEvent.pointerUp(stage, { button: 0, pointerId: 3, clientX: 700, clientY: 500 });
    await fireEvent.doubleClick(stage, { clientX: 700, clientY: 500 });
    expect(api.exportPng).not.toHaveBeenCalled();
    expect(rendered.getByText("400 × 300 px")).toBeInTheDocument();
    expect(rendered.container.querySelector(".selection-border")).toHaveAttribute("x", "100");
    expect(rendered.container.querySelector(".selection-border")).toHaveAttribute("y", "100");

    await fireEvent.click(rendered.getByRole("button", { name: "铅笔" }));
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 4, clientX: 300, clientY: 250 });
    await fireEvent.pointerUp(stage, { button: 0, pointerId: 4, clientX: 300, clientY: 250 });
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 5, clientX: 301, clientY: 251 });
    await fireEvent.pointerUp(stage, { button: 0, pointerId: 5, clientX: 301, clientY: 251 });
    await fireEvent.doubleClick(stage, { clientX: 301, clientY: 251 });
    await waitFor(() => expect(api.exportPng).toHaveBeenCalledWith(
      { sessionId: "session-1", action: "copy" },
      expect.any(Uint8Array),
    ));
    expect(rendered.getByRole("button", { name: "撤销" })).toBeDisabled();
  });
});
