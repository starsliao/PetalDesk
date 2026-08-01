import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_TOOL_SETTINGS, type LongCaptureStatus, type ScreenshotApi } from "../screenshot";
import ScreenshotTool from "./ScreenshotTool.svelte";

const tauri = vi.hoisted(() => ({
  listen: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  currentWindow: {
    outerPosition: vi.fn().mockResolvedValue({ x: -800, y: 0 }),
    innerSize: vi.fn().mockResolvedValue({ width: 800, height: 600 }),
    scaleFactor: vi.fn().mockResolvedValue(1.5),
    onResized: vi.fn().mockResolvedValue(vi.fn()),
    onMoved: vi.fn().mockResolvedValue(vi.fn()),
    onScaleChanged: vi.fn().mockResolvedValue(vi.fn()),
    destroy: vi.fn().mockResolvedValue(undefined),
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => tauri.currentWindow }));

const drawImageSpy = vi.fn();

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
    drawImage: drawImageSpy,
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

type ScreenshotApiMock = ScreenshotApi & Record<
  | "present"
  | "exportPng"
  | "saveToolSettings"
  | "getLongCaptureCapability"
  | "startLongCapture"
  | "pauseLongCapture"
  | "resumeLongCapture"
  | "retryLongCapture"
  | "undoLongCapture"
  | "finishLongCapture"
  | "cancelLongCapture"
  | "cancelLongCaptureSession"
  | "getLongCaptureStatus"
  | "getLongCaptureTile"
  | "exportLongCapture"
  | "prepareLongCaptureAnnotationExport"
  | "uploadLongCaptureAnnotationStrip"
  | "finishLongCaptureAnnotationExport"
  | "cancelLongCaptureAnnotationExport",
  ReturnType<typeof vi.fn>
>;

function longStatus(state: LongCaptureStatus["state"], patch: Partial<LongCaptureStatus> = {}): LongCaptureStatus {
  return {
    jobId: "long-1",
    sessionId: "session-1",
    state,
    engine: "browserEnhanced",
    frameCount: 2,
    width: 400,
    height: 1200,
    canUndo: true,
    ...patch,
  };
}

function mockApi(): ScreenshotApiMock {
  return {
    getSession: vi.fn().mockResolvedValue({
      id: "session-1",
      monitor: { x: -800, y: 0, width: 800, height: 600, scaleFactor: 1.5 },
      frameWidth: 800,
      frameHeight: 600,
      capturedAt: "2026-07-27T01:02:03Z",
    }),
    getFrame: vi.fn().mockResolvedValue(Uint8Array.from([137, 80, 78, 71])),
    present: vi.fn().mockResolvedValue(undefined),
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
    getLongCaptureCapability: vi.fn().mockResolvedValue({ available: true, engines: ["browserEnhanced", "wheel"] }),
    startLongCapture: vi.fn().mockResolvedValue(longStatus("capturing")),
    pauseLongCapture: vi.fn().mockResolvedValue(longStatus("paused", { message: "已暂停" })),
    resumeLongCapture: vi.fn().mockResolvedValue(longStatus("capturing")),
    retryLongCapture: vi.fn().mockResolvedValue(longStatus("capturing")),
    undoLongCapture: vi.fn().mockResolvedValue(longStatus("paused", { frameCount: 1 })),
    finishLongCapture: vi.fn().mockResolvedValue(longStatus("ready", { frameCount: 5, height: 4000 })),
    cancelLongCapture: vi.fn().mockResolvedValue(longStatus("canceled")),
    cancelLongCaptureSession: vi.fn().mockResolvedValue(longStatus("canceled")),
    getLongCaptureStatus: vi.fn().mockResolvedValue(longStatus("capturing")),
    getLongCaptureTile: vi.fn().mockResolvedValue(Uint8Array.from([137, 80, 78, 71])),
    exportLongCapture: vi.fn().mockResolvedValue({ action: "copy" }),
    prepareLongCaptureAnnotationExport: vi.fn().mockResolvedValue({ canceled: false, ticket: "long-ticket", stripHeight: 1024 }),
    uploadLongCaptureAnnotationStrip: vi.fn().mockResolvedValue(undefined),
    finishLongCaptureAnnotationExport: vi.fn().mockResolvedValue({ action: "copy" }),
    cancelLongCaptureAnnotationExport: vi.fn().mockResolvedValue(undefined),
  };
}

beforeEach(() => {
  drawImageSpy.mockReset();
  tauri.listeners.clear();
  tauri.listen.mockReset();
  tauri.listen.mockImplementation(async (name: string, callback: (event: { payload: unknown }) => void) => {
    tauri.listeners.set(name, callback);
    return vi.fn();
  });
  vi.stubGlobal("URL", Object.assign(class extends URL {}, {
    createObjectURL: vi.fn(() => "blob:long-tile"),
    revokeObjectURL: vi.fn(),
  }));
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
  it("keeps the native window hidden until the screenshot frame is painted", async () => {
    const api = mockApi();
    let resolveFrame: ((frame: Uint8Array) => void) | undefined;
    api.getFrame = vi.fn(() => new Promise<Uint8Array>((resolve) => {
      resolveFrame = resolve;
    }));
    render(ScreenshotTool, { api });

    await waitFor(() => expect(api.getFrame).toHaveBeenCalledWith("session-1"));
    expect(api.present).not.toHaveBeenCalled();

    resolveFrame?.(Uint8Array.from([137, 80, 78, 71]));
    await waitFor(() => expect(api.present).toHaveBeenCalledWith("session-1"));
    expect(drawImageSpy).toHaveBeenCalled();
    expect(drawImageSpy.mock.invocationCallOrder[0]).toBeLessThan(api.present.mock.invocationCallOrder[0]);
  });

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
    await waitFor(() => expect(api.present).toHaveBeenCalledWith("session-1"));

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

  it("clears the ordinary screenshot busy state when saving fails", async () => {
    const api = mockApi();
    api.exportPng.mockRejectedValueOnce(new Error("保存窗口不可用"));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "保存截图" }));

    await waitFor(() => expect(rendered.getByText("保存窗口不可用")).toBeInTheDocument());
    expect(rendered.getByRole("button", { name: "保存截图" })).toBeEnabled();
    expect(rendered.getByRole("button", { name: "取消截图" })).toBeEnabled();
  });

  it("starts manual long capture from the selection center and previews only visible image tiles", async () => {
    const api = mockApi();
    api.getLongCaptureStatus.mockResolvedValue(longStatus("paused"));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());

    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalledWith({
      sessionId: "session-1",
      selection: { x: 100, y: 100, width: 400, height: 300 },
      scrollAnchor: { x: 300, y: 250 },
      scope: "selection",
      mode: "manual",
    }));
    expect(rendered.getByText(/2 帧/)).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledWith("long-1"));
    await waitFor(() => expect(rendered.getByRole("button", { name: "继续长截图" })).toBeInTheDocument());
    await fireEvent.click(rendered.getByRole("button", { name: "完成长截图" }));

    await waitFor(() => expect(rendered.getByRole("region", { name: "长截图预览" })).toBeInTheDocument());
    await waitFor(() => expect(api.getLongCaptureTile).toHaveBeenCalledWith("long-1", 0, 1024));
    await waitFor(() => expect(rendered.container.querySelectorAll(".long-preview-content canvas").length).toBeGreaterThan(0));
    expect(rendered.container.querySelectorAll(".long-preview-content canvas").length).toBeLessThanOrEqual(3);
    expect(Array.from(rendered.container.querySelectorAll<HTMLCanvasElement>(".long-preview-content canvas"))
      .every((canvas) => canvas.height <= 1024)).toBe(true);

    await fireEvent.click(rendered.getByRole("button", { name: "长截图形状" }));
    const longCanvas = rendered.getByRole("application", { name: "长截图标注画布" });
    await fireEvent.pointerDown(longCanvas, { button: 0, pointerId: 8, clientX: 50, clientY: 100 });
    await fireEvent.pointerMove(longCanvas, { pointerId: 8, clientX: 200, clientY: 300 });
    await fireEvent.pointerUp(longCanvas, { pointerId: 8, clientX: 200, clientY: 300 });
    expect(rendered.getByRole("button", { name: "撤销长截图标注" })).toBeEnabled();

    await fireEvent.click(rendered.getByRole("button", { name: "复制长截图" }));
    await waitFor(() => expect(api.finishLongCaptureAnnotationExport).toHaveBeenCalledWith("long-ticket"));
    expect(api.prepareLongCaptureAnnotationExport).toHaveBeenCalledWith("long-1", "copy");
    expect(api.uploadLongCaptureAnnotationStrip.mock.calls.map(([, y]) => y)).toEqual([0, 1024, 2048, 3072]);
    expect(api.exportLongCapture).not.toHaveBeenCalled();
    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());
    expect(api.cancel).not.toHaveBeenCalled();
  });

  it("keeps automatic long-capture modes behind the mode menu", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await waitFor(() => expect(rendered.getByRole("button", { name: "选择长截图模式" })).toBeEnabled());

    await fireEvent.click(rendered.getByRole("button", { name: "选择长截图模式" }));
    expect(rendered.getByLabelText("长截图模式")).toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "选择长截图模式" })).toHaveAttribute("aria-controls", "long-mode-options");
    await fireEvent.click(rendered.getByRole("button", { name: "从顶部自动" }));
    expect(rendered.getByRole("toolbar", { name: "选择自动滚动区域" })).toBeInTheDocument();
    expect(api.startLongCapture).not.toHaveBeenCalled();

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 2, clientX: 220, clientY: 180 });

    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalledWith({
      sessionId: "session-1",
      selection: { x: 100, y: 100, width: 400, height: 300 },
      scrollAnchor: { x: 220, y: 180 },
      scope: "selection",
      mode: "top",
    }));
  });

  it("dismisses the long-capture mode menu without changing the selection", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await waitFor(() => expect(rendered.getByRole("button", { name: "选择长截图模式" })).toBeEnabled());

    await fireEvent.click(rendered.getByRole("button", { name: "选择长截图模式" }));
    expect(rendered.getByLabelText("长截图模式")).toBeInTheDocument();
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 2, clientX: 700, clientY: 500 });
    await fireEvent.pointerUp(stage, { pointerId: 2, clientX: 700, clientY: 500 });

    expect(rendered.queryByLabelText("长截图模式")).not.toBeInTheDocument();
    expect(rendered.getByText("400 × 300 px")).toBeInTheDocument();
    expect(rendered.container.querySelector(".selection-border")).toHaveAttribute("x", "100");
    expect(rendered.container.querySelector(".selection-border")).toHaveAttribute("y", "100");
    expect(api.startLongCapture).not.toHaveBeenCalled();
  });

  it("cancels automatic anchor selection locally and restores manual as the default", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await waitFor(() => expect(rendered.getByRole("button", { name: "选择长截图模式" })).toBeEnabled());

    await fireEvent.click(rendered.getByRole("button", { name: "选择长截图模式" }));
    await fireEvent.click(rendered.getByRole("button", { name: "当前位置自动" }));
    expect(rendered.getByRole("toolbar", { name: "选择自动滚动区域" })).toBeInTheDocument();

    const contextMenu = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
    });
    stage.dispatchEvent(contextMenu);

    expect(contextMenu.defaultPrevented).toBe(true);
    await waitFor(() => expect(rendered.queryByRole("toolbar", { name: "选择自动滚动区域" })).not.toBeInTheDocument());
    expect(api.cancelLongCaptureSession).not.toHaveBeenCalled();
    expect(api.cancelLongCapture).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalledWith(expect.objectContaining({
      mode: "manual",
      scrollAnchor: { x: 300, y: 250 },
    })));
  });

  it("starts an automatic capture from the selection center with Enter", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "选择长截图模式" }));
    await fireEvent.click(rendered.getByRole("button", { name: "当前位置自动" }));
    expect(rendered.getByRole("button", { name: "选区中心" })).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Enter" });

    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalledWith(expect.objectContaining({
      mode: "current",
      scrollAnchor: { x: 300, y: 250 },
    })));
  });

  it("resets an automatic mode when annotation confirmation is dismissed", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "形状" }));
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 2, clientX: 180, clientY: 170 });
    await fireEvent.pointerMove(stage, { pointerId: 2, clientX: 280, clientY: 240 });
    await fireEvent.pointerUp(stage, { pointerId: 2, clientX: 280, clientY: 240 });

    await fireEvent.click(rendered.getByRole("button", { name: "选择长截图模式" }));
    await fireEvent.click(rendered.getByRole("button", { name: "从顶部自动" }));
    expect(rendered.getByRole("alertdialog", { name: "清除标注并开始长截图？" })).toBeInTheDocument();
    await fireEvent.keyDown(window, { key: "Escape" });
    expect(rendered.queryByRole("alertdialog", { name: "清除标注并开始长截图？" })).not.toBeInTheDocument();
    expect(api.cancelLongCaptureSession).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "选择长截图模式" }));
    expect(rendered.getByRole("button", { name: "手动滚动（默认）" })).toHaveClass("active");
    expect(rendered.getByRole("button", { name: "从顶部自动" })).not.toHaveClass("active");
  });

  it("does not duplicate an embedded long-capture control action from the window shortcut", async () => {
    const api = mockApi();
    let resolvePause: ((status: LongCaptureStatus) => void) | undefined;
    let resolveResume: ((status: LongCaptureStatus) => void) | undefined;
    api.pauseLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolvePause = resolve;
    }));
    api.resumeLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolveResume = resolve;
    }));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(rendered.getByRole("button", { name: "暂停长截图" })).toBeInTheDocument());

    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledOnce());
    await fireEvent.keyDown(window, { key: " ", code: "Space" });
    expect(api.pauseLongCapture).toHaveBeenCalledOnce();

    resolvePause?.(longStatus("paused"));
    const resumeButton = await waitFor(() => rendered.getByRole("button", { name: "继续长截图" }));
    await fireEvent.keyDown(window, { key: " ", code: "Space" });
    await waitFor(() => expect(api.resumeLongCapture).toHaveBeenCalledOnce());
    expect(resumeButton).toBeDisabled();
    await fireEvent.click(resumeButton);
    expect(api.resumeLongCapture).toHaveBeenCalledOnce();
    resolveResume?.(longStatus("capturing"));
  });

  it("lets embedded Escape cancel while another long-capture action is pending", async () => {
    const api = mockApi();
    let resolvePause: ((status: LongCaptureStatus) => void) | undefined;
    let resolveCancel: ((status: LongCaptureStatus) => void) | undefined;
    api.pauseLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolvePause = resolve;
    }));
    api.cancelLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolveCancel = resolve;
    }));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(rendered.getByRole("button", { name: "暂停长截图" })).toBeInTheDocument());

    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledOnce());
    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledOnce());

    resolvePause?.(longStatus("paused"));
    await Promise.resolve();
    await fireEvent.keyDown(window, { key: " ", code: "Space" });
    expect(api.resumeLongCapture).not.toHaveBeenCalled();

    resolveCancel?.(longStatus("canceled"));
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());
    expect(rendered.queryByRole("button", { name: "继续长截图" })).not.toBeInTheDocument();
  });

  it("invalidates a pending control action before context-menu cancellation", async () => {
    const api = mockApi();
    let resolvePause: ((status: LongCaptureStatus) => void) | undefined;
    let resolveCancel: ((status: LongCaptureStatus) => void) | undefined;
    api.pauseLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolvePause = resolve;
    }));
    api.cancelLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolveCancel = resolve;
    }));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(rendered.getByRole("button", { name: "暂停长截图" })).toBeInTheDocument());
    await fireEvent.click(rendered.getByRole("button", { name: "暂停长截图" }));
    await waitFor(() => expect(api.pauseLongCapture).toHaveBeenCalledOnce());

    const firstContextMenu = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
    });
    stage.dispatchEvent(firstContextMenu);
    expect(firstContextMenu.defaultPrevented).toBe(true);
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledOnce());
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();

    const secondContextMenu = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
    });
    stage.dispatchEvent(secondContextMenu);
    expect(secondContextMenu.defaultPrevented).toBe(false);
    expect(api.cancelLongCapture).toHaveBeenCalledOnce();

    resolvePause?.(longStatus("paused"));
    await Promise.resolve();
    await fireEvent.keyDown(window, { key: " ", code: "Space" });
    expect(api.resumeLongCapture).not.toHaveBeenCalled();
    resolveCancel?.(longStatus("canceled"));
  });

  it("returns to the normal screenshot editor when long capture fails", async () => {
    const api = mockApi();
    api.startLongCapture.mockResolvedValue(longStatus("failed", {
      message: "页面滚动期间失去响应",
      frameCount: 2,
      canUndo: true,
    }));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(rendered.getByText("长截图失败")).toBeInTheDocument());
    expect(rendered.getByText("页面滚动期间失去响应")).toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "重试当前段" })).not.toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "回退上一段" })).not.toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "完成长截图" })).not.toBeInTheDocument();

    await fireEvent.keyDown(window, { key: "Enter" });
    expect(api.finishLongCapture).not.toHaveBeenCalled();
    await fireEvent.click(rendered.getByRole("button", { name: "返回普通截图" }));
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());
    expect(api.retryLongCapture).not.toHaveBeenCalled();
    expect(api.undoLongCapture).not.toHaveBeenCalled();
  });

  it("recovers the normal editor when starting a long capture times out", async () => {
    const api = mockApi();
    let resolveStart: ((status: LongCaptureStatus) => void) | undefined;
    api.startLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolveStart = resolve;
    }));
    const rendered = render(ScreenshotTool, { api, longStartTimeoutMs: 30 });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(api.cancelLongCaptureSession).toHaveBeenCalledWith("session-1"));
    await waitFor(() => expect(rendered.getByText("长截图启动超时，已恢复普通截图。")).toBeInTheDocument());
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();
    expect(api.cancelLongCapture).not.toHaveBeenCalled();

    resolveStart?.(longStatus("capturing"));
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    expect(api.cancelLongCapture).toHaveBeenCalledOnce();
    expect(rendered.queryByRole("button", { name: "暂停长截图" })).not.toBeInTheDocument();
  });

  it("cancels a pending start and ignores its late response", async () => {
    const api = mockApi();
    let resolveStart: ((status: LongCaptureStatus) => void) | undefined;
    api.startLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolveStart = resolve;
    }));
    const rendered = render(ScreenshotTool, { api, longStartTimeoutMs: 1_000 });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalledOnce());

    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(api.cancelLongCaptureSession).toHaveBeenCalledWith("session-1"));
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());

    resolveStart?.(longStatus("capturing"));
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledWith("long-1"));
    expect(api.cancelLongCapture).toHaveBeenCalledOnce();
    expect(rendered.queryByRole("button", { name: "暂停长截图" })).not.toBeInTheDocument();
  });

  it("handles embedded Escape once and ignores late events and poll results after cancel", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    const api = mockApi();
    let resolvePoll: ((status: LongCaptureStatus | null) => void) | undefined;
    api.getLongCaptureStatus.mockImplementation(() => new Promise((resolve) => {
      resolvePoll = resolve;
    }));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(tauri.listeners.has("long_capture_progress")).toBe(true));
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(api.getLongCaptureStatus).toHaveBeenCalledWith("long-1"), { timeout: 2_500 });

    await fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(api.cancelLongCapture).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());

    tauri.listeners.get("long_capture_progress")?.({ payload: longStatus("capturing") });
    resolvePoll?.(longStatus("capturing"));
    await Promise.resolve();

    expect(rendered.queryByRole("button", { name: "暂停长截图" })).not.toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();
  });

  it("returns to the normal editor when polling finds that the active job disappeared", async () => {
    const api = mockApi();
    api.getLongCaptureStatus.mockResolvedValue(null);
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(api.getLongCaptureStatus).toHaveBeenCalledWith("long-1"), { timeout: 2_500 });
    await waitFor(() => expect(rendered.getByText("长截图任务已结束，请重新开始。")).toBeInTheDocument());
    expect(api.cancelLongCaptureSession).toHaveBeenCalledWith("session-1");
    expect(api.cancelLongCapture).not.toHaveBeenCalled();
    expect(rendered.queryByRole("button", { name: "暂停长截图" })).not.toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();
  });

  it("recovers immediately when polling reports that the active job was replaced", async () => {
    const api = mockApi();
    api.getLongCaptureStatus.mockRejectedValue(new Error("长截图任务不存在或已被替换"));
    api.cancelLongCapture.mockRejectedValue(new Error("长截图任务不存在或已被替换"));
    const rendered = render(ScreenshotTool, { api, longPollIntervalMs: 10 });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(rendered.getByText("长截图任务已失效，请重新开始。")).toBeInTheDocument());
    expect(api.getLongCaptureStatus).toHaveBeenCalledOnce();
    expect(api.cancelLongCaptureSession).toHaveBeenCalledWith("session-1");
    expect(api.cancelLongCapture).not.toHaveBeenCalled();
    expect(rendered.queryByRole("button", { name: "暂停长截图" })).not.toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();
  });

  it("bounds generic polling failures and restores the normal editor", async () => {
    const api = mockApi();
    api.getLongCaptureStatus.mockRejectedValue(new Error("IPC connection failed"));
    const rendered = render(ScreenshotTool, {
      api,
      longPollIntervalMs: 10,
      longPollRetryLimit: 2,
    });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(api.getLongCaptureStatus).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(rendered.getByText("无法连接长截图任务，已恢复普通截图。")).toBeInTheDocument());
    expect(api.cancelLongCaptureSession).toHaveBeenCalledWith("session-1");
    expect(api.cancelLongCapture).not.toHaveBeenCalled();
    expect(rendered.queryByRole("button", { name: "暂停长截图" })).not.toBeInTheDocument();
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();
  });

  it("times out a stalled status request and eventually restores the normal editor", async () => {
    const api = mockApi();
    api.getLongCaptureStatus.mockImplementation(() => new Promise(() => undefined));
    const rendered = render(ScreenshotTool, {
      api,
      longPollIntervalMs: 10,
      longPollTimeoutMs: 20,
      longPollRetryLimit: 2,
    });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));

    await waitFor(() => expect(api.getLongCaptureStatus).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(rendered.getByText("无法连接长截图任务，已恢复普通截图。")).toBeInTheDocument());
    expect(api.cancelLongCaptureSession).toHaveBeenCalledWith("session-1");
    expect(api.cancelLongCapture).not.toHaveBeenCalled();
    expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled();
  });

  it("requires confirmation before clearing annotations for a long screenshot", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "形状" }));
    await fireEvent.pointerDown(stage, { button: 0, pointerId: 2, clientX: 180, clientY: 170 });
    await fireEvent.pointerMove(stage, { pointerId: 2, clientX: 280, clientY: 240 });
    await fireEvent.pointerUp(stage, { pointerId: 2, clientX: 280, clientY: 240 });
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());

    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    expect(rendered.getByRole("alertdialog", { name: "清除标注并开始长截图？" })).toBeInTheDocument();
    expect(api.startLongCapture).not.toHaveBeenCalled();
    await fireEvent.click(rendered.getByRole("button", { name: "保留标注" }));
    expect(rendered.getByRole("button", { name: "撤销" })).toBeEnabled();

    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await fireEvent.click(rendered.getByRole("button", { name: "清除并继续" }));
    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalledOnce());
    expect(api.startLongCapture).toHaveBeenCalledWith(expect.objectContaining({
      scrollAnchor: { x: 300, y: 250 },
      mode: "manual",
    }));
  });

  it("accepts long-capture progress events from the desktop backend", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(tauri.listeners.has("long_capture_ready")).toBe(true));
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await waitFor(() => expect(rendered.getByRole("button", { name: "长截图" })).toBeEnabled());
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalled());

    tauri.listeners.get("long_capture_ready")?.({ payload: longStatus("ready", { frameCount: 8, height: 6400 }) });
    await waitFor(() => expect(rendered.getByRole("region", { name: "长截图预览" })).toBeInTheDocument());
    expect(rendered.getByText("400 × 6,400 px")).toBeInTheDocument();
    await fireEvent.click(rendered.getByRole("button", { name: "复制长截图" }));
    await waitFor(() => expect(api.exportLongCapture).toHaveBeenCalledWith("long-1", "copy"));
    expect(api.prepareLongCaptureAnnotationExport).not.toHaveBeenCalled();
  });

  it("keeps a canceled long screenshot save editable", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    const api = mockApi();
    api.exportLongCapture.mockResolvedValueOnce({ action: "save", canceled: true });
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(tauri.listeners.has("long_capture_ready")).toBe(true));
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(api.startLongCapture).toHaveBeenCalled());

    tauri.listeners.get("long_capture_ready")?.({ payload: longStatus("ready", { frameCount: 8, height: 6400 }) });
    await waitFor(() => expect(rendered.getByRole("region", { name: "长截图预览" })).toBeInTheDocument());
    await fireEvent.click(rendered.getByRole("button", { name: "保存长截图" }));

    await waitFor(() => expect(api.exportLongCapture).toHaveBeenCalledWith("long-1", "save"));
    expect(rendered.getByRole("button", { name: "保存长截图" })).toBeEnabled();
    expect(rendered.getByRole("region", { name: "长截图预览" })).toBeInTheDocument();
  });

  it("keeps a terminal ready event when an older finish response arrives late", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    const api = mockApi();
    let resolveFinish: ((status: LongCaptureStatus) => void) | undefined;
    api.finishLongCapture.mockImplementation(() => new Promise<LongCaptureStatus>((resolve) => {
      resolveFinish = resolve;
    }));
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(tauri.listeners.has("long_capture_ready")).toBe(true));
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.click(rendered.getByRole("button", { name: "长截图" }));
    await waitFor(() => expect(rendered.getByRole("button", { name: "完成长截图" })).toBeEnabled());

    await fireEvent.keyDown(window, { key: "Enter" });
    await waitFor(() => expect(api.finishLongCapture).toHaveBeenCalledOnce());
    tauri.listeners.get("long_capture_ready")?.({
      payload: longStatus("ready", { frameCount: 8, height: 6400 }),
    });
    await waitFor(() => expect(rendered.getByRole("region", { name: "长截图预览" })).toBeInTheDocument());
    expect(rendered.getByRole("button", { name: "复制长截图" })).toBeEnabled();

    resolveFinish?.(longStatus("paused", { frameCount: 2 }));
    await Promise.resolve();
    await Promise.resolve();
    expect(rendered.getByRole("region", { name: "长截图预览" })).toBeInTheDocument();
    expect(rendered.queryByRole("button", { name: "继续长截图" })).not.toBeInTheDocument();
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

  it("cancels when right-clicking before a selection is created", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    const contextMenu = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 240,
    });
    stage.dispatchEvent(contextMenu);

    expect(contextMenu.defaultPrevented).toBe(true);
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("session-1"));
  });

  it("cancels outside the selection but preserves the existing menu inside it", async () => {
    const api = mockApi();
    const rendered = render(ScreenshotTool, { api });
    const stage = rendered.getByTestId("screenshot-tool");
    await waitFor(() => expect(api.getFrame).toHaveBeenCalled());

    await fireEvent.pointerDown(stage, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    await fireEvent.pointerMove(stage, { pointerId: 1, clientX: 500, clientY: 400 });
    await fireEvent.pointerUp(stage, { pointerId: 1, clientX: 500, clientY: 400 });

    const insideContextMenu = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
    });
    stage.dispatchEvent(insideContextMenu);
    expect(insideContextMenu.defaultPrevented).toBe(false);
    expect(api.cancel).not.toHaveBeenCalled();

    const outsideContextMenu = new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: 700,
      clientY: 500,
    });
    stage.dispatchEvent(outsideContextMenu);
    expect(outsideContextMenu.defaultPrevented).toBe(true);
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("session-1"));
  });
});
