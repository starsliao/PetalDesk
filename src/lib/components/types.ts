export type NoteColor = "yellow" | "pink" | "blue" | "green" | "purple" | "gray" | "charcoal";

export interface NoteListItem {
  id: string;
  title: string;
  preview?: string;
  excerpt?: string;
  color: NoteColor;
  pinned: boolean;
  createdAt?: string;
  updatedAt: string;
}

export interface TrashListItem extends NoteListItem {
  deletedAt: string;
}

export const NOTE_COLOR_OPTIONS: ReadonlyArray<{
  value: NoteColor;
  label: string;
  hex: string;
}> = [
  { value: "yellow", label: "黄色", hex: "#fff1a8" },
  { value: "pink", label: "粉色", hex: "#ffdce5" },
  { value: "blue", label: "蓝色", hex: "#d9ecff" },
  { value: "green", label: "绿色", hex: "#dff3dc" },
  { value: "purple", label: "紫色", hex: "#eadfff" },
  { value: "gray", label: "灰色", hex: "#e8eaed" },
  { value: "charcoal", label: "炭笔", hex: "#666666" },
];
