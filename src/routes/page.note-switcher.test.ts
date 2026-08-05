import { afterEach, describe, expect, it, vi } from "vitest";

let activeUnmount: (() => void) | null = null;

afterEach(() => {
  activeUnmount?.();
  activeUnmount = null;
  window.history.replaceState({}, "", "/");
  vi.restoreAllMocks();
});

describe("independent note window switcher", () => {
  it("loads the latest note list on demand and opens the selected note window", async () => {
    vi.resetModules();
    window.history.replaceState({}, "", "/?note=note-current");
    const [{ fireEvent, render, waitFor }, { notesApi }, { default: Page }] = await Promise.all([
      import("@testing-library/svelte"),
      import("$lib/bridge"),
      import("./+page.svelte"),
    ]);
    const createdAt = "2026-08-03T08:00:00.000Z";
    vi.spyOn(notesApi, "appInfo").mockResolvedValue({
      workspacePath: "E:/notes",
      version: "0.6.0",
      defaultEditorMode: "typora",
      trayShortcutSettings: {
        doubleClick: "firstNote",
        altDoubleClick: "gantt",
        ctrlDoubleClick: "mfa",
        shiftDoubleClick: "mainWindow",
      },
      protectSensitiveWindows: false,
    });
    vi.spyOn(notesApi, "getNote").mockResolvedValue({
      id: "note-current",
      revision: 1,
      contentHash: "current-hash",
      markdown: "当前正文",
      meta: {
        id: "note-current",
        title: "当前便签",
        editorMode: "typora",
        color: "yellow",
        pinned: false,
        readOnly: false,
        createdAt,
        updatedAt: createdAt,
        schemaVersion: 3,
      },
    });
    const listNotes = vi.spyOn(notesApi, "listNotes").mockResolvedValue([
      {
        id: "note-current",
        title: "当前便签",
        editorMode: "typora",
        color: "yellow",
        pinned: false,
        readOnly: false,
        createdAt,
        updatedAt: createdAt,
        schemaVersion: 3,
        excerpt: "当前正文",
        revision: 1,
      },
      {
        id: "note-other",
        title: "另一张便签",
        editorMode: "plain",
        color: "blue",
        pinned: true,
        readOnly: false,
        createdAt,
        updatedAt: "2026-08-03T09:00:00.000Z",
        schemaVersion: 3,
        excerpt: "另一张正文",
        revision: 2,
      },
    ]);
    const openNoteWindow = vi.spyOn(notesApi, "openNoteWindow").mockResolvedValue(undefined);

    const rendered = render(Page);
    activeUnmount = () => rendered.unmount();
    await waitFor(() => expect(rendered.getByRole("button", { name: "便签列表" })).toBeInTheDocument());

    expect(listNotes).not.toHaveBeenCalled();
    await fireEvent.click(rendered.getByRole("button", { name: "便签列表" }));
    await waitFor(() => expect(rendered.getByRole("menuitem", { name: /另一张便签/ })).toBeInTheDocument());
    expect(listNotes).toHaveBeenCalledWith();
    expect(rendered.getByRole("menuitem", { name: "当前便签" })).toHaveAttribute(
      "aria-current",
      "page",
    );

    await fireEvent.click(rendered.getByRole("menuitem", { name: /另一张便签/ }));
    expect(openNoteWindow).toHaveBeenCalledWith("note-other");
  }, 60_000);
});
