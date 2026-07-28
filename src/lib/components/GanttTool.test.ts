import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { dateHourToOrdinal, type GanttApi, type GanttTask, type UpsertGanttTaskRequest } from "../gantt";
import GanttTool from "./GanttTool.svelte";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function ganttTask(overrides: Partial<GanttTask> = {}): GanttTask {
  return {
    id: "550e8400-e29b-41d4-a716-446655440000",
    name: "设计首版",
    progress: 35,
    startDate: "2026-07-14",
    startHour: 0,
    endDate: "2026-07-20",
    endHour: 23,
    createdAt: "2026-07-14T08:00:00Z",
    updatedAt: "2026-07-14T08:00:00Z",
    ...overrides,
  };
}

function mockApi(initial: GanttTask[] = []): GanttApi & {
  list: ReturnType<typeof vi.fn>;
  upsert: ReturnType<typeof vi.fn>;
  delete: ReturnType<typeof vi.fn>;
  reorder: ReturnType<typeof vi.fn>;
} {
  let items = structuredClone(initial);
  return {
    isDesktop: () => false,
    list: vi.fn().mockImplementation(async () => structuredClone(items)),
    upsert: vi.fn().mockImplementation(async (request: UpsertGanttTaskRequest) => {
      const existing = items.find((item) => item.id === request.id);
      const saved = ganttTask({
        ...existing,
        ...request,
        id: request.id ?? "new-task-id",
        updatedAt: "2026-07-14T09:00:00Z",
      });
      const index = items.findIndex((item) => item.id === saved.id);
      if (index >= 0) items[index] = saved;
      else items.push(saved);
      return structuredClone(saved);
    }),
    delete: vi.fn().mockImplementation(async (id: string) => {
      items = items.filter((item) => item.id !== id);
    }),
    reorder: vi.fn().mockImplementation(async (ids: string[]) => {
      const byId = new Map(items.map((item) => [item.id, item]));
      items = ids.map((id) => byId.get(id)!);
    }),
  };
}

function pointerEvent(type: string, values: Record<string, number>): Event {
  const event = new Event(type, { bubbles: true, cancelable: true });
  for (const [key, value] of Object.entries(values)) {
    Object.defineProperty(event, key, { configurable: true, value });
  }
  return event;
}

function domRect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    x: left,
    y: top,
    top,
    right: left + width,
    bottom: top + height,
    left,
    width,
    height,
    toJSON: () => ({}),
  };
}

describe("GanttTool", () => {
  it("renders the three-column table and a shared date axis", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await waitFor(() => expect(rendered.getByText("设计首版", { selector: ".task-name-display" })).toBeInTheDocument());

    expect(rendered.getByRole("columnheader", { name: "任务名称" })).toBeInTheDocument();
    expect(rendered.getByRole("columnheader", { name: "进度" })).toBeInTheDocument();
    expect(rendered.getByRole("columnheader", { name: "时间轴" })).toBeInTheDocument();
    expect(rendered.container.querySelector(".titlebar-filter .status-filter")).toBeInTheDocument();
    expect(rendered.container.querySelector(".titlebar-filter")?.nextElementSibling).toHaveClass("window-actions");
    expect(rendered.container.querySelector('[title="2026-07-14"]')).toHaveClass("today");
    expect(rendered.getByLabelText("任务进度“设计首版”")).toHaveValue("35");
    expect(rendered.getByLabelText("任务进度“设计首版”")).toHaveAttribute("aria-valuetext", "35%");
    expect(rendered.queryByText("35%")).not.toBeInTheDocument();
    expect(rendered.getByRole("slider", { name: "调整“设计首版”的开始日期" })).toHaveAttribute(
      "aria-valuetext",
      "2026-07-14",
    );
    expect(rendered.getByRole("slider", { name: "调整“设计首版”的结束日期" })).toHaveAttribute(
      "aria-valuetext",
      "2026-07-20",
    );
  });

  it("creates a task for today through the titlebar action", async () => {
    const api = mockApi();
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await waitFor(() => expect(api.list).toHaveBeenCalledOnce());

    await fireEvent.click(rendered.getAllByRole("button", { name: "新建任务" })[0]);
    await waitFor(() => expect(api.upsert).toHaveBeenCalledOnce());
    expect(api.upsert).toHaveBeenCalledWith({
      id: null,
      name: "新任务",
      progress: 0,
      startDate: "2026-07-14",
      startHour: 0,
      endDate: "2026-07-20",
      endHour: 23,
    });
    expect(rendered.getByDisplayValue("新任务")).toBeInTheDocument();
  });

  it("auto-saves name and progress edits and normalizes an empty name on blur", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14", saveDelayMs: 10_000 });
    const display = await rendered.findByText("设计首版", { selector: ".task-name-display" });
    await fireEvent.contextMenu(display.closest(".task-cell")!);
    await fireEvent.click(rendered.getByRole("menuitem", { name: "编辑" }));
    const name = await rendered.findByDisplayValue("设计首版");

    await fireEvent.input(name, { target: { value: "交付安装包" } });
    expect(api.upsert).not.toHaveBeenCalled();
    await fireEvent.blur(name);
    await waitFor(() => expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({
      id: ganttTask().id,
      name: "交付安装包",
    })));

    api.upsert.mockClear();
    const progress = rendered.getByLabelText("任务进度“交付安装包”");
    await fireEvent.input(progress, { target: { value: "68" } });
    expect(progress).toHaveAttribute("aria-valuetext", "68%");
    await fireEvent.blur(progress);
    await waitFor(() => expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({ progress: 68 })));

    api.upsert.mockClear();
    const editedDisplay = await rendered.findByText("交付安装包", { selector: ".task-name-display" });
    await fireEvent.contextMenu(editedDisplay.closest(".task-cell")!);
    await fireEvent.click(rendered.getByRole("menuitem", { name: "编辑" }));
    const secondName = await rendered.findByDisplayValue("交付安装包");
    await fireEvent.input(secondName, { target: { value: "" } });
    await fireEvent.blur(secondName);
    await waitFor(() => expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({ name: "新任务" })));
  });

  it("shrinks task names, wraps to two lines and exposes a tooltip only when truncated", async () => {
    const shrinkName = "ABCDEFGHIJKLMN";
    const longName = "这是一个需要显示两行并且仍然可能放不下的超长任务名称用于测试完整提示";
    const api = mockApi([
      ganttTask({ id: "shrink", name: shrinkName }),
      ganttTask({ id: "wrap", name: longName }),
    ]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    const shrink = await rendered.findByText(shrinkName, { selector: ".task-name-display" });
    const wrap = await rendered.findByText(longName, { selector: ".task-name-display" });
    Object.defineProperty(shrink, "clientWidth", { configurable: true, value: 90 });
    Object.defineProperty(wrap, "clientWidth", { configurable: true, value: 90 });
    window.dispatchEvent(new Event("resize"));

    await waitFor(() => expect(wrap).toHaveClass("two-lines", "name-truncated"));
    expect(shrink).not.toHaveClass("two-lines");
    expect(Number(shrink.style.getPropertyValue("--name-font-size").replace("px", ""))).toBeLessThan(13);
    expect(shrink).not.toHaveAttribute("title", shrinkName);
    expect(wrap).toHaveAttribute("title", longName);
  });

  it("moves handles and the complete date range with the keyboard", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14", saveDelayMs: 10_000 });
    const start = await rendered.findByRole("slider", { name: "调整“设计首版”的开始日期" });

    await fireEvent.keyDown(start, { key: "ArrowRight" });
    await fireEvent.blur(start);
    await waitFor(() => expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({
      startDate: "2026-07-15",
      endDate: "2026-07-20",
    })));

    api.upsert.mockClear();
    const range = rendered.getByRole("slider", { name: "平移“设计首版”的日期范围" });
    await fireEvent.keyDown(range, { key: "ArrowRight", shiftKey: true });
    await fireEvent.blur(range);
    await waitFor(() => expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({
      startDate: "2026-07-22",
      endDate: "2026-07-27",
    })));
  });

  it("persists a pointer drag only after it ends", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    const range = await rendered.findByRole("slider", { name: "平移“设计首版”的日期范围" });
    vi.useFakeTimers();

    await fireEvent(range, pointerEvent("pointerdown", { button: 0, pointerId: 7, clientX: 100 }));
    await fireEvent(range, pointerEvent("pointermove", { pointerId: 7, clientX: 128 }));
    expect(api.upsert).not.toHaveBeenCalled();
    await fireEvent(range, pointerEvent("pointerup", { pointerId: 7, clientX: 128 }));
    expect(api.upsert).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(121);
    expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({
      startDate: "2026-07-15",
      endDate: "2026-07-21",
    }));
  });

  it("pans from the date header and blank timeline without changing task dates", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    const root = rendered.getByTestId("gantt-tool");
    const axis = rendered.getByRole("columnheader", { name: "时间轴" });
    const blankTimeline = rendered.getByRole("cell", { name: "设计首版时间轴" });
    const table = rendered.getByLabelText("甘特任务表格");
    Object.defineProperty(table, "scrollWidth", { configurable: true, value: 1_900 });
    Object.defineProperty(table, "clientWidth", { configurable: true, value: 680 });
    const initialStart = Number(axis.dataset.axisStartHour);

    table.scrollLeft = 100;
    await fireEvent(axis, pointerEvent("pointerdown", { button: 0, pointerId: 11, clientX: 300 }));
    expect(root).toHaveClass("axis-panning");
    await fireEvent(root, pointerEvent("pointermove", { pointerId: 11, clientX: 244 }));
    expect(table.scrollLeft).toBe(156);
    expect(Number(axis.dataset.axisStartHour)).toBe(initialStart);
    await fireEvent(root, pointerEvent("pointerup", { pointerId: 11, clientX: 244 }));
    expect(root).not.toHaveClass("axis-panning");

    table.scrollLeft = 0;
    await fireEvent(blankTimeline, pointerEvent("pointerdown", { button: 0, pointerId: 12, clientX: 200 }));
    await fireEvent(root, pointerEvent("pointermove", { pointerId: 12, clientX: 256 }));
    expect(Number(axis.dataset.axisStartHour)).toBe(initialStart - 2 * 24);
    await fireEvent(root, pointerEvent("pointercancel", { pointerId: 12, clientX: 256 }));
    expect(Number(axis.dataset.axisStartHour)).toBe(initialStart);
    expect(table.scrollLeft).toBe(0);

    await fireEvent(blankTimeline, pointerEvent("pointerdown", { button: 0, pointerId: 13, clientX: 200 }));
    await fireEvent(root, pointerEvent("pointermove", { pointerId: 13, clientX: 256 }));
    await fireEvent(root, pointerEvent("pointerup", { pointerId: 13, clientX: 256 }));
    expect(Number(axis.dataset.axisStartHour)).toBe(initialStart - 2 * 24);
    expect(api.upsert).not.toHaveBeenCalled();
  });

  it("zooms one granularity at a time around the cursor and reaches hour level", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    const axis = rendered.getByRole("columnheader", { name: "时间轴" });
    const timeline = rendered.getByRole("cell", { name: "设计首版时间轴" });
    vi.spyOn(timeline, "getBoundingClientRect").mockReturnValue(domRect(0, 0, 1_568, 46));

    const initialStart = Number(axis.dataset.axisStartHour);
    const cursorAnchor = initialStart + 56 * 24 * .5;
    await fireEvent.wheel(timeline, { deltaY: -120, clientX: 784 });
    expect(axis).toHaveAttribute("data-unit-hours", "12");
    expect(Number(axis.dataset.axisStartHour) + 56 * 12 * .5).toBe(cursorAnchor);
    expect(axis.querySelectorAll(".axis-day")).toHaveLength(56);
    await fireEvent.wheel(timeline, { deltaY: -120, clientX: 784 });
    expect(axis).toHaveAttribute("data-unit-hours", "12");

    const zoomIn = rendered.getByRole("button", { name: "放大时间轴" });
    await fireEvent.click(zoomIn);
    await fireEvent.click(zoomIn);
    await fireEvent.click(zoomIn);
    expect(axis).toHaveAttribute("data-unit-hours", "1");
    expect(zoomIn).toBeDisabled();
    expect(axis.getAttribute("title")).toContain("1 小时");
  });

  it("uses the visible timeline center for toolbar zoom in a narrow viewport", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    const axis = rendered.getByRole("columnheader", { name: "时间轴" });
    const table = rendered.getByLabelText("甘特任务表格");
    const progressHeader = rendered.getByRole("columnheader", { name: "进度" });
    vi.spyOn(axis, "getBoundingClientRect").mockReturnValue(domRect(330, 100, 1_568, 50));
    vi.spyOn(table, "getBoundingClientRect").mockReturnValue(domRect(10, 100, 670, 300));
    vi.spyOn(progressHeader, "getBoundingClientRect").mockReturnValue(domRect(230, 100, 88, 50));

    const ratio = ((330 + 680) / 2 - 330) / 1_568;
    const beforeAnchor = Number(axis.dataset.axisStartHour) + ratio * 56 * 24;
    await fireEvent.click(rendered.getByRole("button", { name: "放大时间轴" }));
    const afterAnchor = Number(axis.dataset.axisStartHour) + ratio * 56 * 12;
    expect(Math.abs(afterAnchor - beforeAnchor)).toBeLessThanOrEqual(6);
  });

  it("finishes and saves a drag after its handle is clipped out of the viewport", async () => {
    const api = mockApi([ganttTask()]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    const start = await rendered.findByRole("slider", { name: "调整“设计首版”的开始日期" });
    const root = rendered.getByTestId("gantt-tool");
    vi.useFakeTimers();

    await fireEvent(start, pointerEvent("pointerdown", { button: 0, pointerId: 9, clientX: 300 }));
    await fireEvent(root, pointerEvent("pointermove", { pointerId: 9, clientX: 0 }));
    expect(rendered.queryByRole("slider", { name: "调整“设计首版”的开始日期" })).not.toBeInTheDocument();
    await fireEvent(root, pointerEvent("pointerup", { pointerId: 9, clientX: 0 }));
    await vi.advanceTimersByTimeAsync(121);

    expect(api.upsert).toHaveBeenCalledWith(expect.objectContaining({
      startDate: "2026-07-03",
      startHour: 0,
      endDate: "2026-07-20",
      endHour: 23,
    }));
  });

  it("pans, resets and locates the earliest task from controls left of new task", async () => {
    const early = ganttTask({ startDate: "2025-01-10", endDate: "2025-01-16" });
    const api = mockApi([early]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    const axis = rendered.getByRole("columnheader", { name: "时间轴" });
    const actions = Array.from(rendered.container.querySelectorAll<HTMLButtonElement>(".window-actions button"));
    const labels = actions.map((button) => button.getAttribute("aria-label"));
    const newTaskIndex = labels.indexOf("新建任务");
    expect(labels.slice(newTaskIndex - 6, newTaskIndex)).toEqual([
      "时间轴向左移动",
      "时间轴向右移动",
      "缩小时间轴",
      "放大时间轴",
      "重置为天级时间轴",
      "定位最早任务",
    ]);

    const beforePan = Number(axis.dataset.axisStartHour);
    await fireEvent.click(rendered.getByRole("button", { name: "时间轴向右移动" }));
    expect(Number(axis.dataset.axisStartHour)).toBe(beforePan + 7 * 24);

    const table = rendered.getByLabelText("甘特任务表格");
    table.scrollLeft = 420;
    await fireEvent.click(rendered.getByRole("button", { name: "定位最早任务" }));
    expect(table.scrollLeft).toBe(0);
    const taskStart = Number(axis.dataset.axisStartHour) + 6 * 24;
    expect(taskStart).toBe(dateHourToOrdinal("2025-01-10", 0));
    expect(rendered.getByRole("slider", { name: "调整“设计首版”的开始日期" })).toBeInTheDocument();

    await fireEvent.click(rendered.getByRole("button", { name: "放大时间轴" }));
    expect(axis).toHaveAttribute("data-unit-hours", "12");
    table.scrollLeft = 300;
    await fireEvent.click(rendered.getByRole("button", { name: "重置为天级时间轴" }));
    expect(table.scrollLeft).toBe(0);
    expect(axis).toHaveAttribute("data-unit-hours", "24");
    expect(Number(axis.dataset.axisStartHour)).toBe(beforePan);
  });

  it("shows month transitions and accurate short bars at week level", async () => {
    const oneDay = ganttTask({ endDate: "2026-07-14", endHour: 23 });
    const api = mockApi([oneDay]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    await fireEvent.click(rendered.getByRole("button", { name: "缩小时间轴" }));
    await fireEvent.click(rendered.getByRole("button", { name: "定位最早任务" }));
    const axis = rendered.getByRole("columnheader", { name: "时间轴" });
    const monthMarkers = Array.from(axis.querySelectorAll(".axis-day span"), (node) => node.textContent);
    expect(axis).toHaveAttribute("data-unit-hours", "168");
    expect(monthMarkers).toContain("8月");
    expect(monthMarkers).toContain("9月");
    expect(rendered.container.querySelector<HTMLElement>(".task-range")?.style.width).toBe("4px");
  });

  it("keeps the axis inside the supported range for a task at the maximum date", async () => {
    const edgeTask = ganttTask({
      startDate: "9999-12-31",
      startHour: 22,
      endDate: "9999-12-31",
      endHour: 23,
    });
    const api = mockApi([edgeTask]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("设计首版", { selector: ".task-name-display" });
    await fireEvent.click(rendered.getByRole("button", { name: "定位最早任务" }));
    const axis = rendered.getByRole("columnheader", { name: "时间轴" });
    expect(Number(axis.dataset.axisEndHour)).toBeLessThanOrEqual(dateHourToOrdinal("9999-12-31", 23) + 1);
    expect(axis.querySelectorAll(".axis-day")).toHaveLength(56);
    expect(rendered.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("confirms before deleting a task", async () => {
    const api = mockApi([ganttTask()]);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    const display = await rendered.findByText("设计首版", { selector: ".task-name-display" });
    await fireEvent.contextMenu(display.closest(".task-cell")!);
    await fireEvent.click(rendered.getByRole("menuitem", { name: "删除" }));
    await waitFor(() => expect(api.delete).toHaveBeenCalledWith(ganttTask().id));
    expect(rendered.queryByText("设计首版", { selector: ".task-name-display" })).not.toBeInTheDocument();
  });

  it("filters by derived progress status and gives completed tasks a distinct treatment", async () => {
    const api = mockApi([
      ganttTask({ id: "todo", name: "整理需求", progress: 0 }),
      ganttTask({ id: "doing", name: "实现界面", progress: 60 }),
      ganttTask({ id: "done", name: "完成设计", progress: 100 }),
    ]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("完成设计", { selector: ".task-name-display" });

    const filterButtons = rendered.container.querySelectorAll<HTMLButtonElement>(".titlebar-filter .status-filter button");
    expect(filterButtons[0]).toHaveAttribute("aria-pressed", "true");
    expect(filterButtons).toHaveLength(4);
    expect(filterButtons[1]).toHaveTextContent(/未开始\s*1/);
    expect(filterButtons[2]).toHaveTextContent(/进行中\s*1/);
    expect(filterButtons[3]).toHaveTextContent(/已完成\s*1/);
    expect(rendered.getByText("完成设计", { selector: ".task-name-display" }).closest(".task-cell")).toHaveClass("status-completed");
    expect(rendered.getByLabelText("已完成")).toBeInTheDocument();

    await fireEvent.click(filterButtons[3]);
    expect(rendered.getByText("完成设计", { selector: ".task-name-display" })).toBeInTheDocument();
    expect(rendered.queryByText("整理需求", { selector: ".task-name-display" })).not.toBeInTheDocument();
    expect(rendered.queryByText("实现界面", { selector: ".task-name-display" })).not.toBeInTheDocument();
  });

  it("reorders only visible task slots while a progress filter is active", async () => {
    const api = mockApi([
      ganttTask({ id: "doing-a", name: "进行任务 A", progress: 20 }),
      ganttTask({ id: "todo", name: "未开始任务", progress: 0 }),
      ganttTask({ id: "doing-b", name: "进行任务 B", progress: 70 }),
      ganttTask({ id: "done", name: "已完成任务", progress: 100 }),
    ]);
    const rendered = render(GanttTool, { api, today: "2026-07-14" });
    await rendered.findByText("进行任务 A", { selector: ".task-name-display" });
    const filterButtons = rendered.container.querySelectorAll<HTMLButtonElement>(".titlebar-filter .status-filter button");
    await fireEvent.click(filterButtons[2]);

    const source = rendered.getByRole("button", { name: "调整任务“进行任务 A”的顺序" });
    const target = rendered.getByRole("button", { name: "调整任务“进行任务 B”的顺序" });
    const sourceCell = source.closest(".task-row")?.querySelector<HTMLElement>(".task-cell");
    const targetCell = target.closest(".task-row")?.querySelector<HTMLElement>(".task-cell");
    expect(sourceCell).toBeTruthy();
    expect(targetCell).toBeTruthy();
    vi.spyOn(sourceCell!, "getBoundingClientRect").mockReturnValue(domRect(0, 100, 220, 46));
    vi.spyOn(targetCell!, "getBoundingClientRect").mockReturnValue(domRect(0, 146, 220, 46));
    vi.spyOn(rendered.getByLabelText("甘特任务表格"), "getBoundingClientRect").mockReturnValue(domRect(0, 0, 680, 500));
    const root = rendered.getByTestId("gantt-tool");
    await fireEvent(source, pointerEvent("pointerdown", { button: 0, pointerId: 21, clientX: 25, clientY: 110 }));
    expect(source.closest(".task-row")).toHaveClass("dragging-row");
    await fireEvent(root, pointerEvent("pointermove", { pointerId: 21, clientX: 25, clientY: 180 }));
    expect(target.closest(".task-row")).toHaveClass("drop-after");
    await fireEvent(root, pointerEvent("pointerup", { pointerId: 21, clientX: 25, clientY: 180 }));

    await waitFor(() => expect(api.reorder).toHaveBeenCalledWith([
      "doing-b",
      "todo",
      "doing-a",
      "done",
    ]));
    const visibleNames = Array.from(rendered.container.querySelectorAll<HTMLElement>(".task-name-display"))
      .map((element) => element.textContent);
    expect(visibleNames).toEqual(["进行任务 B", "进行任务 A"]);
  });
});
