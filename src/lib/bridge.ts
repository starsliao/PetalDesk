import { invoke } from "@tauri-apps/api/core";
import type { EditorMode } from "./editor";
import type { ToolName } from "./tools";
import { NOTE_COLOR_OPTIONS } from "./components/types";
import { previousStorageKey, readMigratedStorageValue } from "./storage";
import {
  DEFAULT_TIMER_DIGIT_OPACITY,
  PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY,
  PREVIOUS_TIMER_STORAGE_KEY,
  TIMER_DIGIT_OPACITY_STORAGE_KEY,
  TIMER_STORAGE_KEY,
  loadTimerDigitOpacity,
  normalizeTimerDigitOpacity,
  parseTimerPersistedState,
  saveTimerDigitOpacity,
  type TimerData,
} from "./timer";

export type NoteColor = "yellow" | "pink" | "blue" | "green" | "purple" | "gray" | "charcoal";
export type { ToolName } from "./tools";

export type TrayShortcutAction =
  | "firstNote"
  | "recentNote"
  | "mainWindow"
  | "timer"
  | "reminder"
  | "gantt"
  | "mfa"
  | "screenshot";

export interface TrayShortcutSettings {
  doubleClick: TrayShortcutAction;
  altDoubleClick: TrayShortcutAction;
  ctrlDoubleClick: TrayShortcutAction;
  shiftDoubleClick: TrayShortcutAction;
}

export const DEFAULT_TRAY_SHORTCUT_SETTINGS: Readonly<TrayShortcutSettings> = {
  doubleClick: "firstNote",
  altDoubleClick: "gantt",
  ctrlDoubleClick: "mfa",
  shiftDoubleClick: "mainWindow",
};

export interface NoteMeta {
  id: string;
  title: string;
  editorMode: EditorMode;
  color: NoteColor;
  pinned: boolean;
  readOnly: boolean;
  createdAt: string;
  updatedAt: string;
  schemaVersion: number;
}

export interface NoteListItem extends NoteMeta {
  excerpt: string;
  revision: number;
}

export interface NoteSnapshot {
  id: string;
  revision: number;
  contentHash: string;
  markdown: string;
  meta: NoteMeta;
}

export interface TrashItem extends NoteListItem {
  deletedAt: string;
}

export interface CommitNoteRequest {
  id: string;
  baseRevision: number;
  baseContentHash?: string;
  markdown: string;
  metaPatch?: Partial<Pick<NoteMeta, "title" | "editorMode" | "color" | "pinned" | "readOnly">>;
}

export interface CommitResult {
  revision: number;
  savedAt: string;
  contentHash: string;
}

export interface AssetRef {
  assetId: string;
  relativePath: string;
}

export interface AppInfo {
  workspacePath: string;
  version: string;
  defaultEditorMode: EditorMode;
  trayShortcutSettings: TrayShortcutSettings;
  protectSensitiveWindows: boolean;
  recoveredDrafts?: number;
  name?: string;
  colors?: string[];
}

export interface AppError {
  code: string;
  message: string;
  details?: string;
}

export interface DataStoragePathResult {
  path: string;
  restartRequired: boolean;
}

const browserKey = "petaldesk.browser-notes.v1";
const previousBrowserKey = previousStorageKey("browser-notes.v1");
export const defaultEditorModeStorageKey = "petaldesk.default-editor-mode.v2";
const previousDefaultEditorModeStorageKey = previousStorageKey("default-editor-mode.v2");
const previousEditorModeStorageKey = previousStorageKey("editor-mode.v1");
const trayShortcutSettingsStorageKey = "petaldesk.tray-shortcut-settings.v1";
export const protectSensitiveWindowsStorageKey = "petaldesk.protect-sensitive-windows.v1";
const editorModes: readonly EditorMode[] = ["typora", "plain"];
const trayShortcutActions: readonly TrayShortcutAction[] = [
  "firstNote",
  "recentNote",
  "mainWindow",
  "timer",
  "reminder",
  "gantt",
  "mfa",
  "screenshot",
];

interface BrowserStore {
  orderSchemaVersion: number;
  notes: NoteSnapshot[];
  trash: Array<NoteSnapshot & { deletedAt: string }>;
}

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function isEditorMode(value: unknown): value is EditorMode {
  return typeof value === "string" && editorModes.includes(value as EditorMode);
}

function normalizeStoredEditorMode(value: unknown): EditorMode {
  return value === "plain" ? "plain" : "typora";
}

function isTrayShortcutAction(value: unknown): value is TrayShortcutAction {
  return typeof value === "string"
    && trayShortcutActions.includes(value as TrayShortcutAction);
}

function normalizeTrayShortcutSettings(value: unknown): TrayShortcutSettings {
  const candidate = typeof value === "object" && value !== null
    ? value as Partial<Record<keyof TrayShortcutSettings, unknown>>
    : {};
  return {
    doubleClick: isTrayShortcutAction(candidate.doubleClick)
      ? candidate.doubleClick
      : DEFAULT_TRAY_SHORTCUT_SETTINGS.doubleClick,
    altDoubleClick: isTrayShortcutAction(candidate.altDoubleClick)
      ? candidate.altDoubleClick
      : DEFAULT_TRAY_SHORTCUT_SETTINGS.altDoubleClick,
    ctrlDoubleClick: isTrayShortcutAction(candidate.ctrlDoubleClick)
      ? candidate.ctrlDoubleClick
      : DEFAULT_TRAY_SHORTCUT_SETTINGS.ctrlDoubleClick,
    shiftDoubleClick: isTrayShortcutAction(candidate.shiftDoubleClick)
      ? candidate.shiftDoubleClick
      : DEFAULT_TRAY_SHORTCUT_SETTINGS.shiftDoubleClick,
  };
}

function readBrowserTrayShortcutSettings(): TrayShortcutSettings {
  if (typeof localStorage === "undefined") return { ...DEFAULT_TRAY_SHORTCUT_SETTINGS };
  try {
    const stored = localStorage.getItem(trayShortcutSettingsStorageKey);
    return normalizeTrayShortcutSettings(stored ? JSON.parse(stored) : null);
  } catch {
    return { ...DEFAULT_TRAY_SHORTCUT_SETTINGS };
  }
}

function readBrowserDefaultEditorMode(): EditorMode {
  if (typeof localStorage === "undefined") return "typora";
  const stored = readMigratedStorageValue(
    localStorage,
    defaultEditorModeStorageKey,
    previousDefaultEditorModeStorageKey,
  ) ?? readMigratedStorageValue(
    localStorage,
    defaultEditorModeStorageKey,
    previousEditorModeStorageKey,
  );
  const migrated = normalizeStoredEditorMode(stored);
  localStorage.setItem(defaultEditorModeStorageKey, migrated);
  return migrated;
}

function readBrowserProtectSensitiveWindows(): boolean {
  if (typeof localStorage === "undefined") return false;
  try {
    return localStorage.getItem(protectSensitiveWindowsStorageKey) === "true";
  } catch {
    return false;
  }
}

function now(): string {
  return new Date().toISOString();
}

function browserContentHash(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `browser-${(hash >>> 0).toString(16).padStart(8, "0")}`;
}

function defaultTimerData(): TimerData {
  return {
    version: 1,
    accumulatedMs: 0,
    runningSince: null,
    logs: [],
    digitOpacity: DEFAULT_TIMER_DIGIT_OPACITY,
  };
}

function readBrowserTimerData(): TimerData {
  if (typeof localStorage === "undefined") return defaultTimerData();
  const state = parseTimerPersistedState(
    readMigratedStorageValue(localStorage, TIMER_STORAGE_KEY, PREVIOUS_TIMER_STORAGE_KEY),
  );
  return {
    ...(state ?? defaultTimerData()),
    digitOpacity: loadTimerDigitOpacity(localStorage),
  };
}

function writeBrowserTimerData(data: TimerData): TimerData {
  const state = {
    version: data.version,
    accumulatedMs: data.accumulatedMs,
    runningSince: data.runningSince,
    logs: data.logs,
  };
  localStorage.setItem(TIMER_STORAGE_KEY, JSON.stringify(state));
  return { ...data, digitOpacity: saveTimerDigitOpacity(localStorage, data.digitOpacity) };
}

function timerDataHasUserState(data: TimerData): boolean {
  return data.accumulatedMs !== 0
    || data.runningSince !== null
    || data.logs.length > 0
    || data.digitOpacity !== DEFAULT_TIMER_DIGIT_OPACITY;
}

/** Map a deterministic seed to the same palette used by the note color picker. */
export function noteColorForSeed(seed: number): NoteColor {
  const normalized = Number.isFinite(seed) ? Math.trunc(seed) : 0;
  const length = NOTE_COLOR_OPTIONS.length;
  const index = ((normalized % length) + length) % length;
  return NOTE_COLOR_OPTIONS[index].value;
}

function noteColorFromId(id: string): NoteColor {
  // UUID v4 has random bits in its first eight hexadecimal characters.
  const entropy = Number.parseInt(id.replaceAll("-", "").slice(0, 8), 16);
  return noteColorForSeed(Number.isFinite(entropy) ? entropy : 0);
}

function stripHtmlForExcerpt(value: string): string {
  if (typeof DOMParser === "undefined") {
    return value
      .replace(/<(script|style|template)\b[^>]*>[\s\S]*?<\/\1\s*>/gi, " ")
      .replace(/<img\b[^>]*>/gi, " [图片] ")
      .replace(/<[^>]+>/g, " ");
  }

  const document = new DOMParser().parseFromString(value, "text/html");
  document.querySelectorAll("script, style, template").forEach((element) => element.remove());
  document.querySelectorAll("img").forEach((image) => {
    image.replaceWith(document.createTextNode(" [图片] "));
  });
  return document.body.textContent ?? "";
}

function titleFromMarkdown(markdown: string): string {
  const first = markdown
    .split(/\r?\n/)
    .map((line) => stripHtmlForExcerpt(line))
    .map((line) => line.replace(/^\s{0,3}(?:#{1,6}|[-*+]>?)\s+/, "").trim())
    .find((line) => Boolean(line) && line !== "[图片]" && !/^!\[[^\]]*\]\([^)]*\)$/.test(line));
  return first || "无标题便签";
}

function excerptFromMarkdown(markdown: string): string {
  return stripHtmlForExcerpt(markdown)
    .replace(/==(?=\S)([^\n]*?\S)==/g, "$1")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "[图片]")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}#{1,6}\s+/gm, "")
    .replace(/^\s{0,3}>\s?/gm, "")
    .replace(/^\s*(?:[-+*]|\d+[.)])\s+/gm, "")
    .replace(/^\s*\[[ xX]\]\s*/gm, "")
    .replace(/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/gm, "")
    .replace(/[`*_>#~-]/g, "")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 120);
}

function excerptFromPlainText(text: string): string {
  return text.replace(/\s+/g, " ").trim().slice(0, 120);
}

function seedStore(): BrowserStore {
  const createdAt = now();
  const make = (
    id: string,
    title: string,
    markdown: string,
    color: NoteColor,
    pinned = false,
  ): NoteSnapshot => ({
    id,
    revision: 1,
    contentHash: browserContentHash(markdown),
    markdown,
    meta: {
      id,
      title,
      editorMode: "typora",
      color,
      pinned,
      readOnly: false,
      createdAt,
      updatedAt: createdAt,
      schemaVersion: 3,
    },
  });

  return {
    orderSchemaVersion: 1,
    notes: [
      make(
        "welcome",
        "欢迎使用飞花 - PetalDesk",
        "# 欢迎使用飞花 - PetalDesk\n\n打开一张便签，写下当下的想法。\n\n- 支持 **Markdown**\n- 自动保存到本地\n- 可以粘贴图片",
        "yellow",
        true,
      ),
      make(
        "today",
        "今天",
        "## 今天\n\n- [x] 整理需求\n- [ ] 完成桌面端原型\n- [ ] 晚上散步",
        "green",
      ),
      make(
        "idea",
        "灵感",
        "> 灵感不必完整，先把它留下。\n\n`Ctrl + N` 随时新建便签。",
        "blue",
      ),
    ],
    trash: [],
  };
}

function readBrowserStore(): BrowserStore {
  if (typeof localStorage === "undefined") return seedStore();
  const value = readMigratedStorageValue(localStorage, browserKey, previousBrowserKey);
  if (!value) {
    const seeded = seedStore();
    localStorage.setItem(browserKey, JSON.stringify(seeded));
    return seeded;
  }
  try {
    const parsed = JSON.parse(value) as Partial<BrowserStore>;
    if (!Array.isArray(parsed.notes)) throw new Error("invalid browser note store");
    let changed = false;
    if (!Array.isArray(parsed.trash)) {
      parsed.trash = [];
      changed = true;
    }
    if (parsed.orderSchemaVersion !== 1) {
      parsed.notes.sort(
        (left, right) => Number(right.meta.pinned) - Number(left.meta.pinned)
          || right.meta.updatedAt.localeCompare(left.meta.updatedAt),
      );
      parsed.orderSchemaVersion = 1;
      changed = true;
    }
    for (const note of [...parsed.notes, ...parsed.trash]) {
      if (!note.meta.title) {
        note.meta.title = titleFromMarkdown(note.markdown);
        changed = true;
      }
      const editorMode = normalizeStoredEditorMode(note.meta.editorMode);
      if (note.meta.editorMode !== editorMode) {
        note.meta.editorMode = editorMode;
        changed = true;
      }
      if (typeof note.meta.readOnly !== "boolean") {
        note.meta.readOnly = false;
        changed = true;
      }
      if (!Number.isInteger(note.meta.schemaVersion) || note.meta.schemaVersion < 3) {
        note.meta.schemaVersion = 3;
        changed = true;
      }
      const contentHash = browserContentHash(note.markdown);
      if (note.contentHash !== contentHash) {
        note.contentHash = contentHash;
        changed = true;
      }
    }
    const migrated = parsed as BrowserStore;
    if (changed) writeBrowserStore(migrated);
    return migrated;
  } catch {
    return seedStore();
  }
}

function writeBrowserStore(store: BrowserStore): void {
  localStorage.setItem(browserKey, JSON.stringify(store));
}

function toListItem(note: NoteSnapshot): NoteListItem {
  return {
    ...note.meta,
    excerpt: note.meta.editorMode === "plain"
      ? excerptFromPlainText(note.markdown)
      : excerptFromMarkdown(note.markdown),
    revision: note.revision,
  };
}

function normalizeError(error: unknown): AppError {
  if (typeof error === "object" && error && "code" in error && "message" in error) {
    return error as AppError;
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as AppError;
      if (parsed.code && parsed.message) return parsed;
    } catch {
      return { code: "unknown", message: error };
    }
  }
  return { code: "unknown", message: "操作失败，请稍后重试。" };
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw normalizeError(error);
  }
}

export const notesApi = {
  isDesktop: isTauriRuntime,

  async appInfo(): Promise<AppInfo> {
    if (isTauriRuntime()) return command<AppInfo>("get_app_info");
    return {
      workspacePath: "浏览器演示数据",
      version: "0.7.3",
      defaultEditorMode: readBrowserDefaultEditorMode(),
      trayShortcutSettings: readBrowserTrayShortcutSettings(),
      protectSensitiveWindows: readBrowserProtectSensitiveWindows(),
      recoveredDrafts: 0,
    };
  },

  async setDefaultEditorMode(defaultEditorMode: EditorMode): Promise<EditorMode> {
    if (!isEditorMode(defaultEditorMode)) {
      throw { code: "invalid_input", message: "不支持的编辑样式。" } satisfies AppError;
    }
    if (isTauriRuntime()) {
      return command<EditorMode>("set_default_editor_mode", { defaultEditorMode });
    }
    localStorage.setItem(defaultEditorModeStorageKey, defaultEditorMode);
    window.dispatchEvent(
      new CustomEvent("petaldesk:default-editor-mode-changed", { detail: defaultEditorMode }),
    );
    return defaultEditorMode;
  },

  async setProtectSensitiveWindows(enabled: boolean): Promise<boolean> {
    const normalized = enabled === true;
    if (isTauriRuntime()) {
      return command<boolean>("set_protect_sensitive_windows", { enabled: normalized });
    }
    localStorage.setItem(protectSensitiveWindowsStorageKey, String(normalized));
    window.dispatchEvent(
      new CustomEvent("petaldesk:protect-sensitive-windows-changed", { detail: normalized }),
    );
    return normalized;
  },

  async setTrayShortcutSettings(
    settings: TrayShortcutSettings,
  ): Promise<TrayShortcutSettings> {
    if (isTauriRuntime()) {
      return command<TrayShortcutSettings>("set_tray_shortcut_settings", { settings });
    }
    const normalized = normalizeTrayShortcutSettings(settings);
    localStorage.setItem(trayShortcutSettingsStorageKey, JSON.stringify(normalized));
    return normalized;
  },

  async listNotes(query = ""): Promise<NoteListItem[]> {
    if (isTauriRuntime()) {
      return query
        ? command<NoteListItem[]>("search_notes", { query })
        : command<NoteListItem[]>("list_notes");
    }
    const needle = query.trim().toLocaleLowerCase();
    return readBrowserStore()
      .notes.map(toListItem)
      .filter((note) => !needle || `${note.title} ${note.excerpt}`.toLocaleLowerCase().includes(needle));
  },

  async getNote(id: string): Promise<NoteSnapshot> {
    if (isTauriRuntime()) return command<NoteSnapshot>("get_note", { noteId: id });
    const note = readBrowserStore().notes.find((item) => item.id === id);
    if (!note) throw { code: "not_found", message: "没有找到这张便签。" } satisfies AppError;
    return structuredClone(note);
  },

  async createNote(): Promise<NoteSnapshot> {
    if (isTauriRuntime()) return command<NoteSnapshot>("create_note");
    const store = readBrowserStore();
    const id = crypto.randomUUID();
    const createdAt = now();
    const note: NoteSnapshot = {
      id,
      revision: 1,
      contentHash: browserContentHash(""),
      markdown: "",
      meta: {
        id,
        title: "无标题便签",
        editorMode: readBrowserDefaultEditorMode(),
        color: noteColorFromId(id),
        pinned: false,
        readOnly: false,
        createdAt,
        updatedAt: createdAt,
        schemaVersion: 3,
      },
    };
    store.notes.push(note);
    writeBrowserStore(store);
    return structuredClone(note);
  },

  async reorderNotes(orderedIds: string[]): Promise<NoteListItem[]> {
    if (isTauriRuntime()) {
      return command<NoteListItem[]>("reorder_notes", { orderedIds });
    }
    const store = readBrowserStore();
    const uniqueIds = new Set(orderedIds);
    if (orderedIds.length !== store.notes.length || uniqueIds.size !== store.notes.length) {
      throw { code: "invalid_input", message: "便签顺序与当前列表不匹配。" } satisfies AppError;
    }
    const byId = new Map(store.notes.map((note) => [note.id, note]));
    const reordered = orderedIds.flatMap((id) => {
      const note = byId.get(id);
      return note ? [note] : [];
    });
    if (reordered.length !== store.notes.length) {
      throw { code: "invalid_input", message: "便签顺序与当前列表不匹配。" } satisfies AppError;
    }
    store.notes = reordered;
    writeBrowserStore(store);
    return store.notes.map(toListItem);
  },

  async commitNote(request: CommitNoteRequest): Promise<CommitResult> {
    if (isTauriRuntime()) return command<CommitResult>("commit_note", { request });
    const store = readBrowserStore();
    const noteIndex = store.notes.findIndex((item) => item.id === request.id);
    const note = store.notes[noteIndex];
    if (!note) throw { code: "not_found", message: "没有找到这张便签。" } satisfies AppError;
    if (
      note.revision !== request.baseRevision
      || (request.baseContentHash !== undefined && note.contentHash !== request.baseContentHash)
    ) {
      throw { code: "conflict", message: "便签已在其他窗口中修改。" } satisfies AppError;
    }
    const metaPatch = { ...request.metaPatch };
    if (metaPatch.editorMode && !isEditorMode(metaPatch.editorMode)) {
      throw { code: "invalid_input", message: "不支持的编辑样式。" } satisfies AppError;
    }
    if (metaPatch.title !== undefined) {
      metaPatch.title = metaPatch.title.trim().slice(0, 200) || "无标题便签";
    }
    note.markdown = request.markdown;
    note.contentHash = browserContentHash(request.markdown);
    note.revision += 1;
    note.meta = { ...note.meta, ...metaPatch, updatedAt: now() };
    if (metaPatch.pinned === true && noteIndex > 0) {
      store.notes.splice(noteIndex, 1);
      store.notes.unshift(note);
    }
    writeBrowserStore(store);
    return { revision: note.revision, savedAt: note.meta.updatedAt, contentHash: note.contentHash };
  },

  async deleteNote(id: string): Promise<void> {
    if (isTauriRuntime()) return command<void>("delete_note", { noteId: id });
    const store = readBrowserStore();
    const index = store.notes.findIndex((item) => item.id === id);
    if (index >= 0) {
      const [note] = store.notes.splice(index, 1);
      store.trash.unshift({ ...note, deletedAt: now() });
      writeBrowserStore(store);
    }
  },

  async listTrash(): Promise<TrashItem[]> {
    if (isTauriRuntime()) {
      const items = await command<Array<NoteListItem & { deletedAt?: string }>>("list_trash");
      return items.map((item) => ({ ...item, deletedAt: item.deletedAt ?? item.updatedAt }));
    }
    return readBrowserStore().trash.map((item) => ({ ...toListItem(item), deletedAt: item.deletedAt }));
  },

  async restoreNote(id: string): Promise<void> {
    if (isTauriRuntime()) return command<void>("restore_note", { noteId: id });
    const store = readBrowserStore();
    const index = store.trash.findIndex((item) => item.id === id);
    if (index >= 0) {
      const [note] = store.trash.splice(index, 1);
      const { deletedAt: _deletedAt, ...snapshot } = note;
      store.notes.push(snapshot);
      writeBrowserStore(store);
    }
  },

  async emptyTrash(): Promise<void> {
    if (isTauriRuntime()) return command<void>("empty_trash");
    const store = readBrowserStore();
    store.trash = [];
    writeBrowserStore(store);
  },

  async importAsset(noteId: string, file: File): Promise<AssetRef> {
    if (!isTauriRuntime()) {
      return { assetId: crypto.randomUUID(), relativePath: URL.createObjectURL(file) };
    }
    const bytes = Array.from(new Uint8Array(await file.arrayBuffer()));
    return command<AssetRef>("import_asset", {
      noteId,
      fileName: file.name,
      mimeType: file.type,
      bytes,
    });
  },

  async readAsset(noteId: string, relativePath: string): Promise<string> {
    if (!isTauriRuntime() || relativePath.startsWith("blob:")) return relativePath;
    const result = await command<{ mime: string; bytes: number[] }>("read_asset", {
      noteId,
      relativePath,
    });
    const data = Uint8Array.from(result.bytes);
    let binary = "";
    for (let index = 0; index < data.length; index += 8192) {
      binary += String.fromCharCode(...data.subarray(index, index + 8192));
    }
    return `data:${result.mime};base64,${btoa(binary)}`;
  },

  async openExternalLink(url: string): Promise<void> {
    const normalized = url.trim();
    if (!/^(?:https?:|mailto:|tel:)/i.test(normalized)) {
      throw { code: "unsafe_link", message: "该链接类型不允许打开。" } satisfies AppError;
    }
    if (isTauriRuntime()) {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(normalized);
      return;
    }
    window.open(normalized, "_blank", "noopener,noreferrer");
  },

  async openNoteWindow(id: string): Promise<void> {
    if (isTauriRuntime()) {
      await command<void>("open_note_window", { noteId: id });
      return;
    }
    const url = new URL(window.location.href);
    url.search = "";
    url.searchParams.set("note", id);
    window.open(url, `petaldesk-note-${id}`, "popup,width=430,height=520");
  },

  async openToolWindow(tool: ToolName): Promise<void> {
    if (isTauriRuntime()) {
      await command<void>("open_tool_window", { tool });
      return;
    }
    const url = new URL(window.location.href);
    url.search = "";
    url.searchParams.set("tool", tool);
    const dimensions =
      tool === "timer"
        ? "popup,width=320,height=140"
        : tool === "reminder"
          ? "popup,width=560,height=620"
          : tool === "gantt"
            ? "popup,width=980,height=600"
            : tool === "mfa"
              ? "popup,width=520,height=640"
              : tool === "passwords"
                ? "popup,width=860,height=650"
              : `popup,width=${Math.max(640, screen.availWidth)},height=${Math.max(480, screen.availHeight)}`;
    window.open(url, `petaldesk-tool-${tool}`, dimensions);
  },

  async closeNoteWindow(id: string): Promise<void> {
    if (isTauriRuntime()) await command<void>("close_note_window", { noteId: id });
  },

  async chooseDataStoragePath(): Promise<DataStoragePathResult | null> {
    if (!isTauriRuntime()) return null;
    const { open } = await import("@tauri-apps/plugin-dialog");
    const selected = await open({ directory: true, multiple: false });
    if (!selected || Array.isArray(selected)) return null;
    return command<DataStoragePathResult>("set_data_storage_path", { path: selected });
  },

  async restartApp(): Promise<void> {
    if (isTauriRuntime()) {
      await command<void>("restart_app");
      return;
    }
    window.location.reload();
  },

  async getTimerData(): Promise<TimerData> {
    if (isTauriRuntime()) return command<TimerData>("get_timer_data");
    return readBrowserTimerData();
  },

  async saveTimerData(data: TimerData): Promise<TimerData> {
    if (isTauriRuntime()) return command<TimerData>("save_timer_data", { data });
    return writeBrowserTimerData(data);
  },

  async migrateLegacyTimerData(): Promise<boolean> {
    if (!isTauriRuntime() || typeof localStorage === "undefined") return false;

    let stateRaw: string | null;
    let opacityRaw: string | null;
    try {
      stateRaw = localStorage.getItem(TIMER_STORAGE_KEY)
        ?? localStorage.getItem(PREVIOUS_TIMER_STORAGE_KEY);
      opacityRaw = localStorage.getItem(TIMER_DIGIT_OPACITY_STORAGE_KEY)
        ?? localStorage.getItem(PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY);
    } catch {
      return false;
    }
    if (stateRaw === null && opacityRaw === null) return false;

    const current = await command<TimerData>("get_timer_data");
    const legacyState = parseTimerPersistedState(stateRaw);
    if (!timerDataHasUserState(current) && (legacyState !== null || opacityRaw !== null)) {
      await command<TimerData>("save_timer_data", {
        data: {
          ...(legacyState ?? defaultTimerData()),
          digitOpacity: opacityRaw === null
            ? DEFAULT_TIMER_DIGIT_OPACITY
            : normalizeTimerDigitOpacity(Number(opacityRaw)),
        } satisfies TimerData,
      });
    }

    try {
      localStorage.removeItem(TIMER_STORAGE_KEY);
      localStorage.removeItem(TIMER_DIGIT_OPACITY_STORAGE_KEY);
      localStorage.removeItem(PREVIOUS_TIMER_STORAGE_KEY);
      localStorage.removeItem(PREVIOUS_TIMER_DIGIT_OPACITY_STORAGE_KEY);
    } catch {
      // The backend is already authoritative even if hardened WebView storage cannot be cleared.
    }
    return true;
  },

  async saveWindowState(): Promise<void> {
    if (!isTauriRuntime()) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const windowHandle = getCurrentWindow();
    const [position, size, scaleFactor, maximized] = await Promise.all([
      windowHandle.outerPosition(),
      windowHandle.innerSize(),
      windowHandle.scaleFactor(),
      windowHandle.isMaximized(),
    ]);
    await command("save_window_state", {
      label: windowHandle.label,
      state: {
        x: position.x / scaleFactor,
        y: position.y / scaleFactor,
        width: size.width / scaleFactor,
        height: size.height / scaleFactor,
        maximized,
      },
    });
  },
};
