import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ConfirmDialog from "./ConfirmDialog.svelte";

afterEach(cleanup);

describe("ConfirmDialog", () => {
  it("does not invoke confirmation until the danger action is chosen", async () => {
    const onconfirm = vi.fn();
    const oncancel = vi.fn();
    const rendered = render(ConfirmDialog, {
      open: true,
      title: "将便签移到回收站？",
      detail: "便签可以稍后恢复。",
      confirmLabel: "移到回收站",
      onconfirm,
      oncancel,
    });

    expect(rendered.getByRole("alertdialog")).toBeInTheDocument();
    expect(onconfirm).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "取消" }));
    expect(oncancel).toHaveBeenCalledOnce();
    expect(onconfirm).not.toHaveBeenCalled();

    await fireEvent.click(rendered.getByRole("button", { name: "移到回收站" }));
    await waitFor(() => expect(onconfirm).toHaveBeenCalledOnce());
  });

  it("cancels on Escape and ignores cancellation while busy", async () => {
    const oncancel = vi.fn();
    const rendered = render(ConfirmDialog, {
      open: true,
      title: "确认",
      busy: true,
      oncancel,
    });

    await fireEvent.keyDown(window, { key: "Escape" });
    expect(oncancel).not.toHaveBeenCalled();
    expect(rendered.getByRole("button", { name: "取消" })).toBeDisabled();
  });

  it("supports a primary confirmation and a custom postpone label", async () => {
    const onconfirm = vi.fn();
    const oncancel = vi.fn();
    const rendered = render(ConfirmDialog, {
      open: true,
      title: "需要重启飞花 - PetalDesk",
      confirmLabel: "立即重启",
      cancelLabel: "稍后",
      tone: "primary",
      onconfirm,
      oncancel,
    });

    expect(rendered.getByRole("button", { name: "立即重启" })).toHaveClass("primary-button");
    await fireEvent.click(rendered.getByRole("button", { name: "稍后" }));
    expect(oncancel).toHaveBeenCalledOnce();
    expect(onconfirm).not.toHaveBeenCalled();
  });
});
