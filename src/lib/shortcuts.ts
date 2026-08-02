const MAC_PLATFORM_PATTERN = /Mac|iPhone|iPad|iPod/i;

function runtimePlatform(): string {
  if (typeof navigator === "undefined") return "";
  return navigator.platform || navigator.userAgent;
}

export function formatShortcut(shortcut: string, platform = runtimePlatform()): string {
  if (!shortcut || !MAC_PLATFORM_PATTERN.test(platform)) return shortcut;

  return shortcut
    .split("+")
    .map((part) => {
      const key = part.trim();
      switch (key.toLowerCase()) {
        case "ctrl":
        case "super":
          return "⌘";
        case "alt":
          return "⌥";
        case "shift":
          return "⇧";
        default:
          return key;
      }
    })
    .join("");
}
