import { afterEach, describe, expect, it, vi } from "vitest";
import type { NoteListItem } from "$lib/bridge";

let activeUnmount: (() => void) | null = null;

afterEach(() => {
  activeUnmount?.();
  activeUnmount = null;
  vi.restoreAllMocks();
  window.history.replaceState({}, "", "/");
});

function note(id: string, title: string): NoteListItem {
  return {
    id,
    title,
    editorMode: "typora",
    color: "yellow",
    pinned: false,
    readOnly: false,
    createdAt: "2026-07-27T08:00:00.000Z",
    updatedAt: "2026-07-27T08:00:00.000Z",
    schemaVersion: 3,
    excerpt: `${title}正文`,
    revision: 1,
  };
}

describe("main note order", () => {
  it("serializes saves while retaining only the latest complete order", async () => {
    vi.resetModules();
    const [{ fireEvent, render, waitFor }, { notesApi }, { default: Page }] = await Promise.all([
      import("@testing-library/svelte"),
      import("$lib/bridge"),
      import("./+page.svelte"),
    ]);
    const initial = [note("a", "甲"), note("b", "乙"), note("c", "丙")];
    vi.spyOn(notesApi, "migrateLegacyTimerData").mockResolvedValue(false);
    vi.spyOn(notesApi, "appInfo").mockResolvedValue({
      workspacePath: "测试目录",
      version: "0.3.5",
      defaultEditorMode: "typora",
    });
    vi.spyOn(notesApi, "listNotes").mockResolvedValue(initial);
    vi.spyOn(notesApi, "listTrash").mockResolvedValue([]);

    let resolveFirst!: (items: NoteListItem[]) => void;
    const firstSave = new Promise<NoteListItem[]>((resolve) => (resolveFirst = resolve));
    const reorder = vi.spyOn(notesApi, "reorderNotes")
      .mockImplementationOnce(() => firstSave)
      .mockImplementationOnce(async (ids) => ids.map((id) => initial.find((item) => item.id === id)!));

    const rendered = render(Page);
    activeUnmount = () => rendered.unmount();
    await rendered.findByRole("button", { name: "调整“甲”的顺序" });
    await fireEvent.keyDown(rendered.getByRole("button", { name: "调整“甲”的顺序" }), { key: "End" });
    expect(reorder).toHaveBeenCalledTimes(1);
    expect(reorder).toHaveBeenNthCalledWith(1, ["b", "c", "a"]);

    await fireEvent.keyDown(rendered.getByRole("button", { name: "调整“乙”的顺序" }), { key: "End" });
    expect(reorder).toHaveBeenCalledTimes(1);
    resolveFirst([initial[1], initial[2], initial[0]]);

    await waitFor(() => expect(reorder).toHaveBeenCalledTimes(2));
    expect(reorder).toHaveBeenNthCalledWith(2, ["c", "a", "b"]);
    await waitFor(() => {
      const titles = Array.from(rendered.container.querySelectorAll<HTMLElement>(".note-card strong"))
        .map((element) => element.textContent);
      expect(titles).toEqual(["丙", "甲", "乙"]);
    });
  }, 60_000);
});
