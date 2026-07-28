import { beforeEach, describe, expect, it, vi } from "vitest";
import { defaultEditorModeStorageKey, noteColorForSeed, notesApi } from "./bridge";
import { previousStorageKey } from "./storage";

const browserNotesKey = "petaldesk.browser-notes.v1";
const previousBrowserNotesKey = previousStorageKey("browser-notes.v1");
const editorModeChangedEvent = "petaldesk:default-editor-mode-changed";

describe("browser note styles", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("maps deterministic seeds across the complete note color palette", () => {
    expect(Array.from({ length: 7 }, (_, index) => noteColorForSeed(index))).toEqual([
      "yellow",
      "pink",
      "blue",
      "green",
      "purple",
      "gray",
      "charcoal",
    ]);
    expect(noteColorForSeed(-1)).toBe("charcoal");
    expect(noteColorForSeed(Number.NaN)).toBe("yellow");
  });

  it("uses the generated note id when choosing a browser fallback color", async () => {
    const uuid = vi.spyOn(crypto, "randomUUID").mockReturnValue("00000001-0000-4000-8000-000000000000");
    try {
      const note = await notesApi.createNote();
      expect(note.meta.color).toBe("pink");
    } finally {
      uuid.mockRestore();
    }
  });

  it("appends new notes and persists a complete manual order", async () => {
    const before = await notesApi.listNotes();
    const created = await notesApi.createNote();
    expect((await notesApi.listNotes()).at(-1)?.id).toBe(created.id);

    const orderedIds = [...before.map((item) => item.id), created.id].reverse();
    await expect(notesApi.reorderNotes(orderedIds)).resolves.toEqual(
      expect.arrayContaining(orderedIds.map((id) => expect.objectContaining({ id }))),
    );
    expect((await notesApi.listNotes()).map((item) => item.id)).toEqual(orderedIds);
    await expect(notesApi.reorderNotes(orderedIds.slice(1))).rejects.toMatchObject({
      code: "invalid_input",
    });
  });

  it("migrates the legacy browser array from its former visible sort exactly once", async () => {
    await notesApi.listNotes();
    const stored = JSON.parse(localStorage.getItem(browserNotesKey)!);
    delete stored.orderSchemaVersion;
    const welcome = stored.notes.find((item: { id: string }) => item.id === "welcome");
    const today = stored.notes.find((item: { id: string }) => item.id === "today");
    const idea = stored.notes.find((item: { id: string }) => item.id === "idea");
    welcome.meta.updatedAt = "2020-01-01T00:00:00.000Z";
    today.meta.updatedAt = "2030-01-01T00:00:00.000Z";
    idea.meta.updatedAt = "2025-01-01T00:00:00.000Z";
    stored.notes = [idea, welcome, today];
    localStorage.setItem(browserNotesKey, JSON.stringify(stored));

    expect((await notesApi.listNotes()).map((item) => item.id)).toEqual(["welcome", "today", "idea"]);
    expect(JSON.parse(localStorage.getItem(browserNotesKey)!).orderSchemaVersion).toBe(1);

    const persisted = JSON.parse(localStorage.getItem(browserNotesKey)!);
    persisted.notes.reverse();
    localStorage.setItem(browserNotesKey, JSON.stringify(persisted));
    expect((await notesApi.listNotes()).map((item) => item.id)).toEqual(["idea", "today", "welcome"]);
  });

  it("moves browser notes from the previous product storage key", async () => {
    await notesApi.listNotes();
    const previous = localStorage.getItem(browserNotesKey)!;
    localStorage.removeItem(browserNotesKey);
    localStorage.setItem(previousBrowserNotesKey, previous);

    expect(await notesApi.listNotes()).toHaveLength(3);
    expect(localStorage.getItem(browserNotesKey)).toBe(previous);
    expect(localStorage.getItem(previousBrowserNotesKey)).toBeNull();
  });

  it("moves a note to the first position when it is pinned", async () => {
    const created = await notesApi.createNote();
    await notesApi.commitNote({
      id: created.id,
      baseRevision: created.revision,
      markdown: created.markdown,
      metaPatch: { pinned: true },
    });

    expect((await notesApi.listNotes())[0].id).toBe(created.id);
  });

  it("defaults to Typora mode and persists a new global selection", async () => {
    expect((await notesApi.appInfo()).version).toBe("0.2.1");
    expect((await notesApi.appInfo()).defaultEditorMode).toBe("typora");

    await expect(notesApi.setDefaultEditorMode("plain")).resolves.toBe("plain");

    expect(localStorage.getItem(defaultEditorModeStorageKey)).toBe("plain");
    expect((await notesApi.appInfo()).defaultEditorMode).toBe("plain");
  });

  it("moves the previous product default editor preference to the new key", async () => {
    const previousKey = previousStorageKey("default-editor-mode.v2");
    localStorage.setItem(previousKey, "plain");

    expect((await notesApi.appInfo()).defaultEditorMode).toBe("plain");
    expect(localStorage.getItem(defaultEditorModeStorageKey)).toBe("plain");
    expect(localStorage.getItem(previousKey)).toBeNull();
  });

  it("notifies other listeners after the editor mode changes", async () => {
    const listener = vi.fn();
    window.addEventListener(editorModeChangedEvent, listener);

    await notesApi.setDefaultEditorMode("plain");

    expect(listener).toHaveBeenCalledOnce();
    expect((listener.mock.calls[0][0] as CustomEvent).detail).toBe("plain");
    window.removeEventListener(editorModeChangedEvent, listener);
  });

  it("migrates a removed legacy mode to Typora", async () => {
    localStorage.setItem(previousStorageKey("editor-mode.v1"), "source");

    expect((await notesApi.appInfo()).defaultEditorMode).toBe("typora");
    expect(localStorage.getItem(defaultEditorModeStorageKey)).toBe("typora");
  });

  it("captures the default style when a note is created", async () => {
    await notesApi.setDefaultEditorMode("plain");
    const note = await notesApi.createNote();
    await notesApi.setDefaultEditorMode("typora");

    expect(note.meta.editorMode).toBe("plain");
    expect((await notesApi.getNote(note.id)).meta.editorMode).toBe("plain");
  });

  it("stores an independent title and note style", async () => {
    const note = await notesApi.createNote();
    await notesApi.commitNote({
      id: note.id,
      baseRevision: note.revision,
      markdown: "### 源码标题\n\n- **预览正文** [链接](https://example.com)",
      metaPatch: { title: "自定义标题", editorMode: "plain" },
    });

    const item = (await notesApi.listNotes()).find((candidate) => candidate.id === note.id)!;
    expect(item.title).toBe("自定义标题");
    expect(item.editorMode).toBe("plain");
    expect(item.excerpt).toBe("### 源码标题 - **预览正文** [链接](https://example.com)");
  });

  it("persists read-only state independently for each note", async () => {
    const note = await notesApi.createNote();
    expect(note.meta.readOnly).toBe(false);

    await notesApi.commitNote({
      id: note.id,
      baseRevision: note.revision,
      markdown: "只读正文",
      metaPatch: { readOnly: true },
    });

    const saved = await notesApi.getNote(note.id);
    const summary = (await notesApi.listNotes()).find((item) => item.id === note.id)!;
    expect(saved.meta.readOnly).toBe(true);
    expect(summary.readOnly).toBe(true);
  });

  it("migrates browser notes created before read-only metadata existed", async () => {
    const note = await notesApi.createNote();
    const stored = JSON.parse(localStorage.getItem(browserNotesKey)!);
    delete stored.notes[0].meta.readOnly;
    stored.notes[0].meta.schemaVersion = 2;
    localStorage.setItem(browserNotesKey, JSON.stringify(stored));

    const migrated = await notesApi.getNote(note.id);
    expect(migrated.meta.readOnly).toBe(false);
    expect(migrated.meta.schemaVersion).toBe(3);
  });

  it("shows rendered plain text for Typora note cards", async () => {
    const note = await notesApi.createNote();
    await notesApi.commitNote({
      id: note.id,
      baseRevision: note.revision,
      markdown: "### 源码标题\n\n- **预览正文** ==高亮内容== [链接](https://example.com)",
      metaPatch: { title: "预览测试", editorMode: "typora" },
    });

    const item = (await notesApi.listNotes()).find((candidate) => candidate.id === note.id)!;
    expect(item.excerpt).toBe("源码标题 预览正文 高亮内容 链接");
  });

  it("rejects unsafe external link protocols", async () => {
    await expect(notesApi.openExternalLink("javascript:alert(1)"))
      .rejects.toMatchObject({ code: "unsafe_link" });
  });

  it("opens each tool in a named browser fallback window", async () => {
    const open = vi.spyOn(window, "open").mockImplementation(() => null);

    await notesApi.openToolWindow("timer");
    await notesApi.openToolWindow("reminder");
    await notesApi.openToolWindow("gantt");
    await notesApi.openToolWindow("screenshot");

    expect(open).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ search: "?tool=timer" }),
      "petaldesk-tool-timer",
      "popup,width=320,height=140",
    );
    expect(open).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ search: "?tool=reminder" }),
      "petaldesk-tool-reminder",
      "popup,width=560,height=620",
    );
    expect(open).toHaveBeenNthCalledWith(
      3,
      expect.objectContaining({ search: "?tool=gantt" }),
      "petaldesk-tool-gantt",
      "popup,width=980,height=600",
    );
    expect(open).toHaveBeenNthCalledWith(
      4,
      expect.objectContaining({ search: "?tool=screenshot" }),
      "petaldesk-tool-screenshot",
      expect.stringMatching(/^popup,width=\d+,height=\d+$/),
    );
  });
});
