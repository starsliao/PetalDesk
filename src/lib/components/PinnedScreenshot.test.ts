import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PinnedScreenshotApi } from "../screenshot";
import PinnedScreenshot from "./PinnedScreenshot.svelte";

function mockApi(): PinnedScreenshotApi & Record<"copy" | "save" | "close", ReturnType<typeof vi.fn>> {
  return {
    getPng: vi.fn().mockResolvedValue(Uint8Array.from([137, 80, 78, 71])),
    copy: vi.fn().mockResolvedValue(undefined),
    save: vi.fn().mockResolvedValue({ savedPath: "C:\\shot.png", canceled: false }),
    close: vi.fn().mockResolvedValue(undefined),
  };
}

beforeEach(() => {
  vi.stubGlobal("URL", {
    ...URL,
    createObjectURL: vi.fn(() => "blob:pinned-shot"),
    revokeObjectURL: vi.fn(),
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("PinnedScreenshot", () => {
  it("loads the in-memory image and exposes copy, save, and close from its context menu", async () => {
    const api = mockApi();
    const onclose = vi.fn();
    const rendered = render(PinnedScreenshot, { pinId: "pin-1", api, onclose });
    await waitFor(() => expect(rendered.getByAltText("置顶截图")).toHaveAttribute("src", "blob:pinned-shot"));
    const root = rendered.getByTestId("pinned-screenshot");

    await fireEvent.contextMenu(root, { clientX: 20, clientY: 20 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: /复制/ }));
    await waitFor(() => expect(api.copy).toHaveBeenCalledWith("pin-1"));

    await fireEvent.contextMenu(root, { clientX: 20, clientY: 20 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: /另存为/ }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith("pin-1"));

    await fireEvent.contextMenu(root, { clientX: 20, clientY: 20 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: /关闭/ }));
    await waitFor(() => expect(api.close).toHaveBeenCalledWith("pin-1"));
    expect(onclose).toHaveBeenCalledOnce();
  });

  it("restores pin controls after the save dialog is canceled", async () => {
    const api = mockApi();
    api.save.mockResolvedValueOnce({ canceled: true });
    const rendered = render(PinnedScreenshot, { pinId: "pin-1", api });
    await waitFor(() => expect(rendered.getByAltText("置顶截图")).toBeInTheDocument());
    const root = rendered.getByTestId("pinned-screenshot");

    await fireEvent.contextMenu(root, { clientX: 20, clientY: 20 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: /另存为/ }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith("pin-1"));

    await fireEvent.contextMenu(root, { clientX: 20, clientY: 20 });
    await fireEvent.click(rendered.getByRole("menuitem", { name: /复制/ }));
    await waitFor(() => expect(api.copy).toHaveBeenCalledWith("pin-1"));
  });
});
