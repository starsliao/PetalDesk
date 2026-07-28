export interface MonitorBounds {
  x: number;
  y: number;
  width: number;
  height: number;
  scaleFactor: number;
}

export interface ScreenshotSession {
  id: string;
  monitor: MonitorBounds;
  frameWidth: number;
  frameHeight: number;
  capturedAt: string;
}

export interface Point {
  x: number;
  y: number;
}

export interface Rect extends Point {
  width: number;
  height: number;
}

export type ResizeHandle = "n" | "ne" | "e" | "se" | "s" | "sw" | "w" | "nw";
export type ScreenshotToolName =
  | "shape"
  | "line"
  | "pencil"
  | "marker"
  | "effect"
  | "text"
  | "eraser";
export type ShapeKind = "rectangle" | "ellipse";
export type LineKind = "line" | "arrow" | "double-arrow";
export type LineStyle = "solid" | "dashed";
export type EffectKind = "mosaic" | "blur";
export type DrawMode = "brush" | "rectangle";
export type MarkerTip = "round" | "square";

export interface StrokeStyle {
  color: string;
  width: number;
  lineStyle: LineStyle;
}

export interface AnnotationBase {
  id: string;
}

export interface ShapeAnnotation extends AnnotationBase {
  kind: "shape";
  shape: ShapeKind;
  rect: Rect;
  stroke: StrokeStyle;
  fill: string | null;
}

export interface LineAnnotation extends AnnotationBase {
  kind: "line";
  line: LineKind;
  from: Point;
  to: Point;
  stroke: StrokeStyle;
}

export interface PathAnnotation extends AnnotationBase {
  kind: "pencil" | "marker";
  points: Point[];
  color: string;
  width: number;
  opacity: number;
  tip: MarkerTip;
}

export interface TextAnnotation extends AnnotationBase {
  kind: "text";
  rect: Rect;
  text: string;
  color: string;
  fontFamily: string;
  fontSize: number;
  bold: boolean;
  italic: boolean;
  underline: boolean;
}

export interface EffectAnnotation extends AnnotationBase {
  kind: "effect";
  effect: EffectKind;
  mode: DrawMode;
  rect: Rect;
  points: Point[];
  size: number;
  intensity: number;
}

export interface EraserAnnotation extends AnnotationBase {
  kind: "eraser";
  mode: DrawMode;
  rect: Rect;
  points: Point[];
  size: number;
}

export type Annotation =
  | ShapeAnnotation
  | LineAnnotation
  | PathAnnotation
  | TextAnnotation
  | EffectAnnotation
  | EraserAnnotation;

export interface ToolSettings {
  shape: ShapeKind;
  line: LineKind;
  lineStyle: LineStyle;
  strokeColor: string;
  fillColor: string | null;
  strokeWidth: number;
  pencilWidth: number;
  markerWidth: number;
  markerTip: MarkerTip;
  effect: EffectKind;
  effectMode: DrawMode;
  effectSize: number;
  effectIntensity: number;
  eraserMode: DrawMode;
  eraserSize: number;
  textColor: string;
  fontFamily: string;
  fontSize: number;
  textBold: boolean;
  textItalic: boolean;
  textUnderline: boolean;
}

export const DEFAULT_TOOL_SETTINGS: ToolSettings = {
  shape: "rectangle",
  line: "arrow",
  lineStyle: "solid",
  strokeColor: "#ff3b30",
  fillColor: null,
  strokeWidth: 4,
  pencilWidth: 4,
  markerWidth: 20,
  markerTip: "round",
  effect: "mosaic",
  effectMode: "brush",
  effectSize: 28,
  effectIntensity: 12,
  eraserMode: "brush",
  eraserSize: 28,
  textColor: "#ff3b30",
  fontFamily: "Microsoft YaHei UI",
  fontSize: 22,
  textBold: false,
  textItalic: false,
  textUnderline: false,
};

export type ColorFormat = "hex" | "rgb";

export interface ScreenshotSettings {
  schemaVersion: number;
  shortcut: string;
  lastSaveDirectory: string | null;
  colorFormat: ColorFormat;
  toolParameters: ToolSettings;
}

export type ScreenshotExportAction = "copy" | "save" | "pin";

export interface ScreenshotExportRequest {
  sessionId: string;
  action: ScreenshotExportAction;
}

export interface PreparedScreenshotExport {
  canceled: boolean;
  ticket?: string | null;
}

export interface ScreenshotExportResult {
  action: ScreenshotExportAction;
  pinId?: string | null;
  savedPath?: string | null;
  canceled?: boolean;
}

export interface ScreenshotApi {
  getSession(sessionId?: string): Promise<ScreenshotSession | null>;
  getFrame(sessionId: string): Promise<Uint8Array>;
  present(sessionId: string): Promise<void>;
  cancel(sessionId?: string): Promise<void>;
  getSettings(): Promise<ScreenshotSettings>;
  setShortcut(shortcut: string): Promise<ScreenshotSettings>;
  saveToolSettings(settings: ToolSettings, colorFormat: ColorFormat): Promise<void>;
  exportPng(request: ScreenshotExportRequest, png: Uint8Array): Promise<ScreenshotExportResult>;
}

export interface PinnedScreenshotApi {
  getPng(pinId: string): Promise<Uint8Array>;
  copy(pinId: string): Promise<void>;
  save(pinId: string): Promise<{ savedPath?: string | null; canceled?: boolean }>;
  close(pinId: string): Promise<void>;
}
