import { describe, expect, it } from "vitest";
import { formatShortcut } from "./shortcuts";

describe("formatShortcut", () => {
  it("uses native macOS modifier symbols", () => {
    expect(formatShortcut("Ctrl+Shift+S", "MacIntel")).toBe("⌘⇧S");
    expect(formatShortcut("Super+Alt+P", "MacIntel")).toBe("⌘⌥P");
  });

  it("keeps standalone keys and non-macOS shortcuts unchanged", () => {
    expect(formatShortcut("F1", "MacIntel")).toBe("F1");
    expect(formatShortcut("Ctrl+Shift+S", "Win32")).toBe("Ctrl+Shift+S");
    expect(formatShortcut("Ctrl+Shift+S", "")).toBe("Ctrl+Shift+S");
  });
});
