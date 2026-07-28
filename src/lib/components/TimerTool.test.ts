import { cleanup, fireEvent, render, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  PREVIOUS_TIMER_STORAGE_KEY,
  TIMER_DIGIT_OPACITY_STORAGE_KEY,
  TIMER_STORAGE_KEY,
  type TimerData,
} from "../timer";
import TimerTool from "./TimerTool.svelte";

const timerBackend = vi.hoisted(() => ({
  data: null as unknown as TimerData,
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: timerBackend.invoke }));

const nativeWindow = vi.hoisted(() => ({
  close: vi.fn(async () => undefined),
  innerSize: vi.fn(async () => ({
    toLogical: () => ({ width: 320, height: 140 }),
  })),
  scaleFactor: vi.fn(async () => 1),
  setMaxSize: vi.fn(async (_size: { width: number; height: number } | null) => undefined),
  setSize: vi.fn(async (_size: { width: number; height: number }) => undefined),
  startDragging: vi.fn(async () => undefined),
  startResizeDragging: vi.fn(async () => undefined),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => nativeWindow,
}));

vi.mock("@tauri-apps/api/dpi", () => ({
  LogicalSize: class LogicalSize {
    constructor(
      public width: number,
      public height: number,
    ) {}
  },
}));

beforeEach(() => {
  window.localStorage.clear();
  vi.clearAllMocks();
  timerBackend.data = {
    version: 1,
    accumulatedMs: 0,
    runningSince: null,
    logs: [],
    digitOpacity: 1,
  };
  timerBackend.invoke.mockImplementation(
    async (command: string, args?: Record<string, unknown>) => {
      if (command === "get_timer_data") return structuredClone(timerBackend.data);
      if (command === "save_timer_data") {
        timerBackend.data = structuredClone(args?.data as TimerData);
        return structuredClone(timerBackend.data);
      }
      throw new Error(`unexpected command: ${command}`);
    },
  );
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 6, 12, 9, 0, 0));
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  window.localStorage.clear();
});

describe("TimerTool", () => {
  it("renders an automatically running, draggable timer with four hover actions", async () => {
    const rendered = render(TimerTool);
    await waitFor(() => expect(rendered.getByLabelText("已计时时间")).toHaveTextContent("00:00"));

    const root = rendered.getByTestId("timer-tool");
    const toolbar = rendered.getByRole("toolbar", { name: "计时器操作" });
    const controlOverlay = rendered.getByTestId("timer-control-overlay");
    const colon = rendered.getByTestId("timer-colon");
    expect(root).toHaveAttribute("aria-label", "计时器");
    expect(root.querySelector("[data-tauri-drag-region]")).toBeInTheDocument();
    expect(rendered.getAllByTestId("seven-segment-digit")).toHaveLength(4);
    expect(rendered.getAllByTestId("seven-segment-digit")[0]).toHaveAttribute("data-digit", "0");
    expect(controlOverlay).toHaveAttribute("data-overlay", "full-card");
    expect(controlOverlay).toHaveClass("full-card-overlay");
    expect(controlOverlay.parentElement).toHaveClass("timer-face");
    expect(within(toolbar).getAllByRole("button")).toHaveLength(4);
    expect(within(toolbar).getByRole("slider", { name: "数字透明度" })).toHaveValue("1");
    expect(within(toolbar).getByRole("button", { name: "关闭计时器" })).toBeInTheDocument();
    expect(colon).toHaveClass("is-running");
    expect(colon).not.toHaveClass("is-paused");
    expect(document.body).toHaveClass("timer-tool-window");

    const segments = root.querySelectorAll(".segment");
    expect(root.querySelectorAll(".segment.is-on").length).toBeGreaterThan(0);
    expect(root.querySelectorAll(".segment:not(.is-on)").length).toBeGreaterThan(0);
    expect(segments).toHaveLength(28);

    await vi.advanceTimersByTimeAsync(61_000);
    expect(rendered.getByLabelText("已计时时间")).toHaveTextContent("00:01");
  });

  it("adjusts digit opacity immediately and restores it from localStorage", async () => {
    const rendered = render(TimerTool);
    const slider = rendered.getByRole("slider", { name: "数字透明度" });
    const display = rendered.getByLabelText("已计时时间");

    expect(slider).toHaveValue("1");
    expect(slider).toHaveAttribute("aria-valuetext", "100%");
    expect(display).toHaveStyle("--digit-opacity: 1");

    await fireEvent.input(slider, { target: { value: "0.55" } });
    expect(slider).toHaveValue("0.55");
    expect(slider).toHaveAttribute("aria-valuetext", "55%");
    expect(display).toHaveStyle("--digit-opacity: 0.55");
    expect(window.localStorage.getItem(TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBe("0.55");

    rendered.unmount();
    const reopened = render(TimerTool);
    expect(reopened.getByRole("slider", { name: "数字透明度" })).toHaveValue("0.55");
    expect(reopened.getByLabelText("已计时时间")).toHaveStyle("--digit-opacity: 0.55");
  });

  it("loads and saves digit opacity through the desktop data backend", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    timerBackend.data.digitOpacity = 0.55;
    const rendered = render(TimerTool);
    const slider = rendered.getByRole("slider", { name: "数字透明度" });

    await vi.dynamicImportSettled();
    await vi.advanceTimersByTimeAsync(0);
    expect(slider).toHaveValue("0.55");
    await fireEvent.input(slider, { target: { value: "0.7" } });

    await vi.dynamicImportSettled();
    await vi.advanceTimersByTimeAsync(0);
    expect(timerBackend.data.digitOpacity).toBe(0.7);
    expect(window.localStorage.getItem(TIMER_DIGIT_OPACITY_STORAGE_KEY)).toBeNull();
  });

  it("pauses, resumes and keeps elapsed time while paused", async () => {
    const rendered = render(TimerTool);
    await waitFor(() => expect(rendered.getByRole("button", { name: "暂停计时" })).toBeInTheDocument());
    await vi.advanceTimersByTimeAsync(65_000);

    await fireEvent.click(rendered.getByRole("button", { name: "暂停计时" }));
    expect(rendered.getByRole("button", { name: "继续计时" })).toBeInTheDocument();
    expect(rendered.getByLabelText("已计时时间")).toHaveTextContent("00:01");
    expect(rendered.getByTestId("timer-colon")).toHaveClass("is-paused");
    expect(rendered.getByTestId("timer-colon")).not.toHaveClass("is-running");

    await vi.advanceTimersByTimeAsync(120_000);
    expect(rendered.getByLabelText("已计时时间")).toHaveTextContent("00:01");

    await fireEvent.click(rendered.getByRole("button", { name: "继续计时" }));
    expect(rendered.getByTestId("timer-colon")).toHaveClass("is-running");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(rendered.getByLabelText("已计时时间")).toHaveTextContent("00:02");
  });

  it("starts at zero and records the restored duration as a reset", async () => {
    window.localStorage.setItem(
      PREVIOUS_TIMER_STORAGE_KEY,
      JSON.stringify({
        version: 1,
        accumulatedMs: 100 * 60 * 60 * 1_000,
        runningSince: null,
        logs: [],
      }),
    );

    const rendered = render(TimerTool);
    await waitFor(() => expect(rendered.getByLabelText("已计时时间")).toHaveTextContent("00:00"));

    const digits = rendered.getAllByTestId("seven-segment-digit");
    expect(digits).toHaveLength(4);
    expect(digits.map((digit) => digit.getAttribute("data-digit"))).toEqual(["0", "0", "0", "0"]);
    expect(rendered.getByTestId("timer-colon")).toHaveClass("is-running");

    await fireEvent.click(rendered.getByRole("button", { name: "展开计时记录" }));
    expect(rendered.getByRole("region", { name: "计时记录" })).toHaveTextContent("100:00:00");
  });

  it("records the pre-reset duration and filters records by action", async () => {
    const rendered = render(TimerTool);
    await waitFor(() => expect(rendered.getByRole("button", { name: "暂停计时" })).toBeInTheDocument());
    await vi.advanceTimersByTimeAsync(65_000);

    await fireEvent.click(rendered.getByRole("button", { name: "暂停计时" }));
    await fireEvent.click(rendered.getByRole("button", { name: "继续计时" }));
    await fireEvent.click(rendered.getByRole("button", { name: "重置计时" }));
    await fireEvent.click(rendered.getByRole("button", { name: "展开计时记录" }));

    const history = rendered.getByRole("region", { name: "计时记录" });
    expect(within(history).getAllByText("00:01:05")).toHaveLength(3);
    expect(within(history).getAllByText("暂停")).toHaveLength(2); // filter and record
    expect(within(history).getAllByText("继续")).toHaveLength(2); // filter and record
    expect(within(history).getAllByText("重置")).toHaveLength(3); // filter and two records

    await fireEvent.click(within(history).getByRole("button", { name: "暂停" }));
    expect(within(history).getAllByText("暂停")).toHaveLength(2);
    expect(within(history).queryByText("继续", { selector: "li span" })).not.toBeInTheDocument();
    expect(within(history).queryByText("重置", { selector: "li span" })).not.toBeInTheDocument();
  });

  it("starts a fresh session without treating an ordinary unmount as window closure", async () => {
    const first = render(TimerTool);
    await waitFor(() => expect(first.getByRole("button", { name: "暂停计时" })).toBeInTheDocument());
    await vi.advanceTimersByTimeAsync(75_000);
    await fireEvent.click(first.getByRole("button", { name: "暂停计时" }));
    await fireEvent.click(first.getByRole("button", { name: "继续计时" }));
    first.unmount();

    const beforeReopen = JSON.parse(window.localStorage.getItem(TIMER_STORAGE_KEY) ?? "{}");
    expect(beforeReopen.logs.map((entry: { action: string }) => entry.action)).toEqual([
      "reset",
      "pause",
      "resume",
    ]);

    await vi.advanceTimersByTimeAsync(45_000);
    const restored = render(TimerTool);
    await waitFor(() => expect(restored.getByLabelText("已计时时间")).toHaveTextContent("00:00"));
    await fireEvent.click(restored.getByRole("button", { name: "展开计时记录" }));

    const history = restored.getByRole("region", { name: "计时记录" });
    expect(within(history).getAllByText("00:01:15")).toHaveLength(2);
    expect(within(history).getByText("00:02:00")).toBeInTheDocument();
  });

  it("records one closing pause before native close and ignores the unload fallback duplicate", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const rendered = render(TimerTool);
    expect(rendered.getByRole("button", { name: "关闭计时器" })).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(5_000);

    await fireEvent.click(rendered.getByRole("button", { name: "关闭计时器" }));
    await vi.dynamicImportSettled();
    expect(nativeWindow.close).toHaveBeenCalledOnce();
    window.dispatchEvent(new Event("beforeunload"));

    const persisted = timerBackend.data;
    expect(persisted.runningSince).toBeNull();
    expect(persisted.logs.map((entry: { action: string }) => entry.action)).toEqual([
      "reset",
      "pause",
    ]);
    expect(persisted.logs.at(-1)?.elapsedMs).toBe(5_000);
  });

  it("restores the user-sized timer before closing an expanded history view", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const rendered = render(TimerTool);

    await fireEvent.click(rendered.getByRole("button", { name: "展开计时记录" }));
    await vi.dynamicImportSettled();
    expect(nativeWindow.setMaxSize).toHaveBeenNthCalledWith(1, null);
    expect(nativeWindow.setSize.mock.calls[0][0]).toMatchObject({ width: 520, height: 400 });
    expect(nativeWindow.setMaxSize.mock.invocationCallOrder[0]).toBeLessThan(
      nativeWindow.setSize.mock.invocationCallOrder[0],
    );

    await fireEvent.click(rendered.getByRole("button", { name: "关闭计时器" }));
    await vi.dynamicImportSettled();

    expect(nativeWindow.setSize.mock.calls[1][0]).toMatchObject({ width: 320, height: 140 });
    expect(nativeWindow.setMaxSize).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ width: 320, height: 194 }),
    );
    expect(nativeWindow.close).toHaveBeenCalledOnce();
    expect(nativeWindow.setSize.mock.invocationCallOrder[1]).toBeLessThan(
      nativeWindow.setMaxSize.mock.invocationCallOrder[1],
    );
    expect(nativeWindow.setMaxSize.mock.invocationCallOrder[1]).toBeLessThan(
      nativeWindow.close.mock.invocationCallOrder[0],
    );
  });

  it("expands both window dimensions, restores them, and clears records after confirmation", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const rendered = render(TimerTool);
    expect(rendered.getByRole("button", { name: "展开计时记录" })).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(3_000);
    await fireEvent.click(rendered.getByRole("button", { name: "暂停计时" }));

    await fireEvent.click(rendered.getByRole("button", { name: "展开计时记录" }));
    await vi.dynamicImportSettled();
    expect(nativeWindow.setMaxSize).toHaveBeenNthCalledWith(1, null);
    expect(nativeWindow.setSize).toHaveBeenCalled();
    expect(nativeWindow.setSize.mock.calls[0][0]).toMatchObject({ width: 520, height: 400 });
    expect(nativeWindow.setMaxSize.mock.invocationCallOrder[0]).toBeLessThan(
      nativeWindow.setSize.mock.invocationCallOrder[0],
    );

    const history = rendered.getByRole("region", { name: "计时记录" });
    await fireEvent.click(within(history).getByRole("button", { name: "清空" }));
    expect(confirm).toHaveBeenCalledWith("确定要清空所有计时记录吗？");
    expect(within(history).getByText("暂无符合条件的记录")).toBeInTheDocument();
    await waitFor(() => expect(timerBackend.data.logs).toEqual([]));

    await fireEvent.click(rendered.getByRole("button", { name: "收起计时记录" }));
    await vi.dynamicImportSettled();
    expect(nativeWindow.setSize).toHaveBeenCalledTimes(2);
    expect(nativeWindow.setSize.mock.calls[1][0]).toMatchObject({ width: 320, height: 140 });
    expect(nativeWindow.setMaxSize).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ width: 320, height: 194 }),
    );
    expect(nativeWindow.setSize.mock.invocationCallOrder[1]).toBeLessThan(
      nativeWindow.setMaxSize.mock.invocationCallOrder[1],
    );
  });

  it("uses explicit native dragging and resizing from the visible timer stack", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const rendered = render(TimerTool);
    expect(rendered.getByTestId("timer-tool")).toBeInTheDocument();

    const face = rendered.getByLabelText("已计时时间").closest(".timer-face");
    expect(face).not.toBeNull();
    await fireEvent.pointerDown(face as Element, { button: 0 });
    await vi.dynamicImportSettled();
    expect(nativeWindow.startDragging).toHaveBeenCalledOnce();

    await fireEvent.pointerDown(rendered.getByRole("button", { name: "调整计时器大小" }), {
      button: 0,
    });
    await vi.dynamicImportSettled();
    expect(nativeWindow.startResizeDragging).toHaveBeenCalledWith("SouthEast");
  });
});
