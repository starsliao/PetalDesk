import { afterEach, describe, expect, it, vi } from "vitest";

type NotesApi = typeof import("$lib/bridge").notesApi;
let activeUnmount: (() => void) | null = null;

afterEach(() => {
  activeUnmount?.();
  activeUnmount = null;
  window.history.replaceState({}, "", "/");
  vi.restoreAllMocks();
});

async function renderNotesList() {
  vi.resetModules();
  window.history.replaceState({}, "", "/");
  const [{ fireEvent, render, waitFor }, { notesApi }, { default: Page }] = await Promise.all([
    import("@testing-library/svelte"),
    import("$lib/bridge"),
    import("./+page.svelte"),
  ]);
  const note = {
    id: "note-1",
    title: "待删除便签",
    editorMode: "typora" as const,
    color: "yellow" as const,
    pinned: false,
    readOnly: false,
    createdAt: "2026-07-26T08:00:00.000Z",
    updatedAt: "2026-07-26T09:00:00.000Z",
    schemaVersion: 1,
    excerpt: "测试正文",
    revision: 1,
  };
  vi.spyOn(notesApi, "appInfo").mockResolvedValue({
    workspacePath: "E:/notes",
    version: "0.3.4",
    defaultEditorMode: "typora",
  });
  vi.spyOn(notesApi, "listNotes").mockResolvedValue([note]);
  vi.spyOn(notesApi, "listTrash").mockResolvedValue([]);
  const deleteNote = vi.spyOn(notesApi, "deleteNote").mockResolvedValue(undefined);
  const rendered = render(Page);
  activeUnmount = () => rendered.unmount();
  await waitFor(() => expect(rendered.getByRole("button", { name: "删除" })).toBeInTheDocument());
  return { deleteNote, fireEvent, notesApi, rendered, waitFor };
}

async function renderActiveNote() {
  vi.resetModules();
  window.history.replaceState({}, "", "/?note=note-1");
  const [{ fireEvent, render, waitFor }, { notesApi }, { default: Page }] = await Promise.all([
    import("@testing-library/svelte"),
    import("$lib/bridge"),
    import("./+page.svelte"),
  ]);
  vi.spyOn(notesApi, "appInfo").mockResolvedValue({
    workspacePath: "E:/notes",
    version: "0.3.4",
    defaultEditorMode: "typora",
  });
  vi.spyOn(notesApi, "getNote").mockResolvedValue({
    id: "note-1",
    revision: 1,
    markdown: "正文",
    meta: {
      id: "note-1",
      title: "独立窗口便签",
      editorMode: "typora",
      color: "blue",
      pinned: false,
      readOnly: false,
      createdAt: "2026-07-26T08:00:00.000Z",
      updatedAt: "2026-07-26T09:00:00.000Z",
      schemaVersion: 1,
    },
  });
  const deleteNote = vi.spyOn(notesApi, "deleteNote").mockRejectedValue(new Error("测试停止关闭窗口"));
  const rendered = render(Page);
  activeUnmount = () => rendered.unmount();
  await waitFor(() => expect(rendered.getByRole("button", { name: "删除便签" })).toBeInTheDocument());
  return { deleteNote, fireEvent, rendered, waitFor };
}

describe("note deletion confirmation", () => {
  it("requires confirmation from the list and cancellation has no side effects", async () => {
    const { deleteNote, fireEvent, rendered, waitFor } = await renderNotesList();
    const nativeConfirm = vi.spyOn(window, "confirm");

    await fireEvent.click(rendered.getByRole("button", { name: "删除" }));
    expect(rendered.getByRole("alertdialog", { name: "将便签移到回收站？" })).toBeInTheDocument();
    expect(rendered.getByText(/待删除便签.*回收站/)).toBeInTheDocument();
    expect(deleteNote).not.toHaveBeenCalled();
    expect(nativeConfirm).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "取消" }));
    expect(rendered.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(deleteNote).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "删除" }));
    await fireEvent.click(rendered.getByRole("button", { name: "移到回收站" }));
    await waitFor(() => expect(deleteNote).toHaveBeenCalledOnce());
    expect(deleteNote).toHaveBeenCalledWith("note-1");
  }, 60_000);

  it("uses the same confirmation flow in an independent note window", async () => {
    const { deleteNote, fireEvent, rendered, waitFor } = await renderActiveNote();
    const nativeConfirm = vi.spyOn(window, "confirm");

    await fireEvent.click(rendered.getByRole("button", { name: "删除便签" }));
    expect(rendered.getByRole("alertdialog", { name: "将便签移到回收站？" })).toBeInTheDocument();
    expect(deleteNote).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "取消" }));
    expect(deleteNote).not.toHaveBeenCalled();
    expect(nativeConfirm).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "删除便签" }));
    await fireEvent.click(rendered.getByRole("button", { name: "移到回收站" }));
    await waitFor(() => expect(deleteNote).toHaveBeenCalledWith("note-1"));
  }, 60_000);
});

describe("data storage path", () => {
  it("offers restart now or later after the backend requests a restart", async () => {
    const { fireEvent, notesApi, rendered, waitFor } = await renderNotesList();
    const chooseDataStoragePath = vi.spyOn(notesApi, "chooseDataStoragePath").mockResolvedValue({
      path: "D:/PetalDesk 数据",
      restartRequired: true,
    });
    const restartApp = vi.spyOn(notesApi, "restartApp").mockResolvedValue(undefined);

    await fireEvent.click(rendered.getByRole("button", { name: "打开设置" }));
    await waitFor(() => expect(rendered.getByRole("dialog", { name: "设置" })).toBeInTheDocument());
    expect(rendered.getByText("E:/notes")).toBeInTheDocument();
    await fireEvent.click(rendered.getByRole("button", { name: "更改" }));
    await waitFor(() => expect(chooseDataStoragePath).toHaveBeenCalledOnce());
    expect(rendered.getByRole("alertdialog", { name: "需要重启飞花 - PetalDesk" })).toBeInTheDocument();
    expect(rendered.getByText(/D:\/PetalDesk 数据/)).toBeInTheDocument();
    expect(restartApp).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "稍后" }));
    expect(rendered.queryByRole("alertdialog", { name: "需要重启飞花 - PetalDesk" })).not.toBeInTheDocument();
    expect(restartApp).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "打开设置" }));
    await waitFor(() => expect(rendered.getByRole("button", { name: "更改" })).toBeInTheDocument());
    await fireEvent.click(rendered.getByRole("button", { name: "更改" }));
    await waitFor(() => expect(rendered.getByRole("button", { name: "立即重启" })).toBeInTheDocument());
    await fireEvent.click(rendered.getByRole("button", { name: "立即重启" }));
    await waitFor(() => expect(restartApp).toHaveBeenCalledOnce());
  }, 60_000);
});
