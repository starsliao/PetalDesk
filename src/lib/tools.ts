export const TOOL_NAMES = ["timer", "reminder", "gantt", "screenshot"] as const;

export type ToolName = (typeof TOOL_NAMES)[number];

export function parseToolName(value: string | null): ToolName | null {
  return TOOL_NAMES.includes(value as ToolName) ? (value as ToolName) : null;
}
