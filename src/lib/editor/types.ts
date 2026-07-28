import type { EditorView } from "@codemirror/view";

export type EditorMode = "typora" | "plain";

export type AssetImporter = (file: File) => Promise<string>;

export interface AssetInsertDetail {
  file: File;
  path: string;
  markdown: string;
}

export interface EditorReadyDetail {
  view: EditorView;
}

export interface EditorErrorDetail {
  operation: "asset-import";
  error: unknown;
}
