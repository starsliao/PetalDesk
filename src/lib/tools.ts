export const TOOL_MENU_ITEMS = [
  { name: "timer", label: "计时器" },
  { name: "reminder", label: "提醒" },
  { name: "gantt", label: "任务甘特图" },
  { name: "mfa", label: "MFA 验证器" },
  { name: "passwords", label: "密码管理器" },
  { name: "screenshot", label: "截图" },
] as const;

export type ToolName = (typeof TOOL_MENU_ITEMS)[number]["name"];
export type ToolMenuItem = (typeof TOOL_MENU_ITEMS)[number];

export const TOOL_NAMES = TOOL_MENU_ITEMS.map(({ name }) => name) as readonly ToolName[];

export function toolMenuItemLabel(item: ToolMenuItem, screenshotShortcut: string): string {
  return item.name === "screenshot" ? `${item.label}(${screenshotShortcut})` : item.label;
}

export function parseToolName(value: string | null): ToolName | null {
  return TOOL_NAMES.includes(value as ToolName) ? (value as ToolName) : null;
}
