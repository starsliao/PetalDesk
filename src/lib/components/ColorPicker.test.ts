import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ColorPicker from "./ColorPicker.svelte";

afterEach(cleanup);

describe("ColorPicker", () => {
  it("offers the charcoal background and reports the selection", async () => {
    const onchange = vi.fn();
    const rendered = render(ColorPicker, {
      value: "yellow",
      onchange,
    });

    expect(rendered.getAllByRole("button")).toHaveLength(7);
    const charcoal = rendered.getByRole("button", { name: "炭笔背景" });
    expect(charcoal).toHaveAttribute("aria-pressed", "false");
    expect(charcoal).toHaveAttribute("title", "炭笔");

    await fireEvent.click(charcoal);

    expect(onchange).toHaveBeenCalledOnce();
    expect(onchange).toHaveBeenCalledWith("charcoal");
  });
});
