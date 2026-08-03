import { cleanup, fireEvent, render, within } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import NotesList from "./NotesList.svelte";
import type { NoteListItem } from "./types";

afterEach(cleanup);

const note: NoteListItem = {
  id: "note-1",
  title: "第一条便签",
  preview: "便签预览",
  color: "yellow",
  pinned: false,
  updatedAt: "2026-07-27T08:00:00.000Z",
};

const secondNote: NoteListItem = {
  ...note,
  id: "note-2",
  title: "第二条便签",
  color: "blue",
  updatedAt: "2026-07-27T09:00:00.000Z",
};

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

describe("NotesList header actions", () => {
  it("keeps settings and trash in the header and replaces header create with tools", () => {
    const { container, getByRole } = render(NotesList, {
      notes: [],
      onsettingsopen: vi.fn(),
      onshowtrash: vi.fn(),
      oncreate: vi.fn(),
      ontoolopen: vi.fn(),
    });

    const header = within(container.querySelector(".list-header") as HTMLElement);
    const settingsButton = header.getByRole("button", { name: "打开设置" });
    expect(settingsButton).toHaveAttribute("data-tooltip", "设置");
    expect(header.getByRole("button", { name: "回收站" })).toBeInTheDocument();
    expect(header.getByRole("button", { name: "小工具" })).toHaveClass("tools-button");
    expect(header.queryByRole("button", { name: "新建便签" })).not.toBeInTheDocument();
    expect(header.queryByLabelText("默认编辑样式")).not.toBeInTheDocument();
    expect(header.queryByRole("button", { name: "修改飞花 - PetalDesk 数据存储路径" })).not.toBeInTheDocument();
    expect(header.queryByLabelText("排序方式")).not.toBeInTheDocument();
    expect(getByRole("button", { name: "新建便签" })).toHaveClass("create-note-tile");
  });

  it("opens all tools, shows the current screenshot shortcut, and reports the selection", async () => {
    const ontoolopen = vi.fn();
    const { getByRole, queryByRole } = render(NotesList, {
      notes: [],
      screenshotShortcut: "F1",
      ontoolopen,
    });

    const toolsButton = getByRole("button", { name: "小工具" });
    expect(toolsButton).toHaveAttribute("aria-expanded", "false");
    await fireEvent.click(toolsButton);

    expect(toolsButton).toHaveAttribute("aria-expanded", "true");
    expect(getByRole("menuitem", { name: "计时器" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "提醒" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "任务甘特图" })).toBeInTheDocument();
    expect(getByRole("menuitem", { name: "密码管理器" })).toBeInTheDocument();
    const screenshot = getByRole("menuitem", { name: "截图(F1)" });
    expect(screenshot).toBeInTheDocument();

    await fireEvent.click(screenshot);

    expect(ontoolopen).toHaveBeenCalledOnce();
    expect(ontoolopen).toHaveBeenCalledWith("screenshot");
    expect(queryByRole("menu", { name: "小工具" })).not.toBeInTheDocument();
  });

  it("closes the tools menu with Escape and an outside click", async () => {
    const { getByRole, queryByRole } = render(NotesList, {
      notes: [],
      ontoolopen: vi.fn(),
    });
    const toolsButton = getByRole("button", { name: "小工具" });

    await fireEvent.click(toolsButton);
    await fireEvent.keyDown(window, { key: "Escape" });
    expect(queryByRole("menu", { name: "小工具" })).not.toBeInTheDocument();

    await fireEvent.click(toolsButton);
    await fireEvent.click(document.body);
    expect(queryByRole("menu", { name: "小工具" })).not.toBeInTheDocument();
  });
});

describe("NotesList manual order", () => {
  it("reorders cards only when their dedicated handle is dragged", async () => {
    const onreorder = vi.fn();
    const rendered = render(NotesList, { notes: [note, secondNote], onreorder, onopen: vi.fn() });
    const sourceHandle = rendered.getByRole("button", { name: "调整“第一条便签”的顺序" });
    const targetCard = rendered.getByRole("article", { name: "第二条便签" });
    vi.spyOn(targetCard, "getBoundingClientRect").mockReturnValue(domRect(250, 0, 230, 104));

    await fireEvent(sourceHandle, pointerEvent("pointerdown", {
      button: 0,
      pointerId: 17,
      clientX: 10,
      clientY: 10,
    }));
    expect(sourceHandle.closest(".note-card")).toHaveClass("dragging");
    await fireEvent(window, pointerEvent("pointermove", {
      pointerId: 17,
      clientX: 300,
      clientY: 50,
    }));
    await fireEvent(window, pointerEvent("pointerup", {
      pointerId: 17,
      clientX: 300,
      clientY: 50,
    }));

    expect(onreorder).toHaveBeenCalledOnce();
    expect(onreorder).toHaveBeenCalledWith(["note-2", "note-1"]);
  });

  it("supports keyboard reordering and disables partial search-result reordering", async () => {
    const onreorder = vi.fn();
    const rendered = render(NotesList, { notes: [note, secondNote], onreorder });
    await fireEvent.keyDown(
      rendered.getByRole("button", { name: "调整“第一条便签”的顺序" }),
      { key: "End" },
    );
    expect(onreorder).toHaveBeenCalledWith(["note-2", "note-1"]);

    await rendered.rerender({ notes: [note], query: "第一条", onreorder });
    expect(rendered.queryByRole("button", { name: "调整“第一条便签”的顺序" })).not.toBeInTheDocument();
  });

  it("cancels an active pointer reorder with Escape", async () => {
    const onreorder = vi.fn();
    const rendered = render(NotesList, { notes: [note, secondNote], onreorder });
    const handle = rendered.getByRole("button", { name: "调整“第一条便签”的顺序" });
    await fireEvent(handle, pointerEvent("pointerdown", {
      button: 0,
      pointerId: 23,
      clientX: 10,
      clientY: 10,
    }));
    expect(handle.closest(".note-card")).toHaveClass("dragging");

    await fireEvent.keyDown(window, { key: "Escape" });
    await fireEvent(window, pointerEvent("pointerup", { pointerId: 23, clientX: 10, clientY: 10 }));

    expect(handle.closest(".note-card")).not.toHaveClass("dragging");
    expect(onreorder).not.toHaveBeenCalled();
  });
});

describe("NotesList create tile", () => {
  it("renders after the final note and creates a note", async () => {
    const oncreate = vi.fn();
    const { getByRole } = render(NotesList, { notes: [note], oncreate });
    const noteCard = getByRole("article", { name: "第一条便签" });
    const createTile = getByRole("button", { name: "新建便签" });

    expect(noteCard.nextElementSibling).toBe(createTile);
    await fireEvent.click(createTile);

    expect(oncreate).toHaveBeenCalledOnce();
  });

  it("uses the same tile for an empty library and hides it in search results", async () => {
    const { getByRole, queryByRole, rerender } = render(NotesList, {
      notes: [],
      oncreate: vi.fn(),
    });

    expect(getByRole("button", { name: "新建便签" })).toHaveClass("create-note-tile");

    await rerender({ notes: [], query: "不存在", oncreate: vi.fn() });
    expect(queryByRole("button", { name: "新建便签" })).not.toBeInTheDocument();
  });
});
