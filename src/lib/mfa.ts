import { invoke } from "@tauri-apps/api/core";

export type MfaAlgorithm = "sha1" | "sha256" | "sha512";
export type MfaRecoveryState = "setup-required" | "ready" | "password-required" | "unavailable";

export interface MfaStatus {
  available: boolean;
  locked: boolean;
  entryCount: number;
  protection:
    | "windows-dpapi"
    | "windows-dpapi-recovery-password"
    | "macos-keychain"
    | "macos-keychain-recovery-password"
    | "browser-demo"
    | "unavailable";
  recoveryState: MfaRecoveryState;
  captureExcluded?: boolean;
  recoveredFromBackup?: boolean;
  message?: string | null;
}

export interface MfaEntrySummary {
  id: string;
  name: string;
  issuer: string;
  accountName: string;
  iconEmoji: string;
  pinned: boolean;
  algorithm: MfaAlgorithm;
  digits: number;
  period: number;
  createdAt: string;
  updatedAt: string;
}

export interface MfaTrashEntrySummary extends MfaEntrySummary {
  deletedAt: string;
}

export interface MfaRevealResult {
  id: string;
  code: string;
  /** Unix milliseconds, Unix seconds, or an ISO timestamp from older backends. */
  validUntil: number | string;
}

export interface MfaEntryExport {
  id: string;
  name: string;
  issuer: string;
  accountName: string;
  iconEmoji: string;
  algorithm: MfaAlgorithm;
  digits: number;
  period: number;
  createdAt: string;
  updatedAt: string;
  secretBase32: string;
  otpauthUri: string;
  qrPngDataUrl: string;
}

export interface MfaImportPreview {
  sessionId: string;
  name: string;
  issuer: string;
  accountName: string;
  iconEmoji?: string | null;
  algorithm: MfaAlgorithm;
  digits: number;
  period: number;
  warnings: string[];
}

export interface MfaUriPreviewError {
  line: number;
  message: string;
}

export interface MfaUriPreviewResult {
  previews: MfaImportPreview[];
  errors: MfaUriPreviewError[];
}

export interface MfaImportCommit {
  sessionId: string;
  iconEmoji: string;
}

export interface MfaManualImportRequest {
  name: string;
  issuer: string;
  accountName: string;
  secret: string;
  iconEmoji: string;
  algorithm: MfaAlgorithm;
  digits: number;
  period: number;
}

export interface MfaEntryUpdateRequest {
  id: string;
  name: string;
  issuer: string;
  accountName: string;
  iconEmoji: string;
}

export interface MfaApi {
  isDesktop(): boolean;
  getStatus(): Promise<MfaStatus>;
  list(): Promise<MfaEntrySummary[]>;
  listTrash(): Promise<MfaTrashEntrySummary[]>;
  scanScreenQr(): Promise<MfaImportPreview[]>;
  previewQrImage(bytes: Uint8Array, mediaType?: string): Promise<MfaImportPreview[]>;
  previewUri(uri: string): Promise<MfaImportPreview[]>;
  previewUris(uris: string): Promise<MfaUriPreviewResult>;
  previewManual(request: MfaManualImportRequest): Promise<MfaImportPreview[]>;
  commitImport(sessionId: string, iconEmoji: string): Promise<MfaEntrySummary>;
  commitImports(imports: MfaImportCommit[]): Promise<MfaEntrySummary[]>;
  cancelImport(sessionId: string): Promise<void>;
  update(request: MfaEntryUpdateRequest): Promise<MfaEntrySummary>;
  delete(id: string): Promise<void>;
  restore(id: string): Promise<MfaEntrySummary>;
  permanentlyDelete(id: string): Promise<void>;
  emptyTrash(): Promise<void>;
  reorder(orderedIds: string[]): Promise<MfaEntrySummary[]>;
  setPinned(id: string, pinned: boolean): Promise<MfaEntrySummary[]>;
  reveal(id: string): Promise<MfaRevealResult>;
  exportEntry(id: string, password: string): Promise<MfaEntryExport>;
  copy(id: string): Promise<void>;
  configureRecoveryPassword(password: string, currentPassword?: string): Promise<void>;
  unlockWithRecoveryPassword(password: string): Promise<void>;
  lock(): Promise<void>;
}

interface BackendError {
  code?: string;
  message?: string;
}

function isDesktopRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function errorMessage(error: unknown): string {
  if (typeof error === "object" && error && "message" in error) {
    return String((error as BackendError).message);
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as BackendError;
      if (parsed.message) return parsed.message;
    } catch {
      return error;
    }
  }
  return "验证器操作失败，请稍后重试。";
}

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new Error(errorMessage(error));
  }
}

function normalizedAlgorithm(value: unknown): MfaAlgorithm {
  const normalized = String(value ?? "sha1").toLowerCase().replaceAll("-", "");
  if (normalized === "sha256") return "sha256";
  if (normalized === "sha512") return "sha512";
  return "sha1";
}

function normalizedPreview(value: MfaImportPreview): MfaImportPreview {
  return {
    ...value,
    name: value.name || value.issuer || value.accountName || "未命名账户",
    issuer: value.issuer ?? "",
    accountName: value.accountName ?? "",
    iconEmoji: value.iconEmoji || "🔐",
    algorithm: normalizedAlgorithm(value.algorithm),
    digits: [6, 7, 8].includes(Number(value.digits)) ? Number(value.digits) : 6,
    period: Math.max(1, Number(value.period) || 30),
    warnings: Array.isArray(value.warnings) ? value.warnings.map(String) : [],
  };
}

function previewList(value: MfaImportPreview | MfaImportPreview[]): MfaImportPreview[] {
  return (Array.isArray(value) ? value : [value]).map(normalizedPreview);
}

function normalizedUriPreviewResult(value: MfaUriPreviewResult): MfaUriPreviewResult {
  return {
    previews: previewList(value.previews ?? []),
    errors: Array.isArray(value.errors)
      ? value.errors.map((item) => ({
        line: Math.max(1, Math.trunc(Number(item.line) || 1)),
        message: String(item.message || "无法识别这一行链接。"),
      }))
      : [],
  };
}

export function validUntilMilliseconds(value: number | string): number {
  if (typeof value === "string") {
    const numeric = Number(value);
    if (Number.isFinite(numeric)) return validUntilMilliseconds(numeric);
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : 0;
  }
  if (!Number.isFinite(value)) return 0;
  // Current Unix seconds are around 1e9, while milliseconds are around 1e12.
  return value > 0 && value < 100_000_000_000 ? value * 1_000 : value;
}

export function secondsUntil(validUntil: number | string, now = Date.now()): number {
  return Math.max(0, Math.ceil((validUntilMilliseconds(validUntil) - now) / 1_000));
}

export function formatOtpCode(code: string, digits = code.length): string {
  const compact = code.replaceAll(/\s/g, "");
  if (!compact) return "";
  const leftLength = Math.floor(Math.max(1, digits) / 2);
  return `${compact.slice(0, leftLength)} ${compact.slice(leftLength)}`.trim();
}

export function maskedOtpCode(digits = 6): string {
  const safeDigits = [6, 7, 8].includes(digits) ? digits : 6;
  return formatOtpCode("•".repeat(safeDigits), safeDigits);
}

function nowIso(): string {
  return new Date().toISOString();
}

function pinnedFirst(items: MfaEntrySummary[]): MfaEntrySummary[] {
  return [
    ...items.filter((entry) => entry.pinned),
    ...items.filter((entry) => !entry.pinned),
  ];
}

function demoCode(id: string, digits: number, period: number): MfaRevealResult {
  const now = Date.now();
  const step = Math.floor(now / (period * 1_000));
  let value = step >>> 0;
  for (const char of id) value = Math.imul(value ^ char.charCodeAt(0), 16_777_619) >>> 0;
  const modulus = 10 ** digits;
  return {
    id,
    code: String(value % modulus).padStart(digits, "0"),
    validUntil: (Math.floor(now / (period * 1_000)) + 1) * period * 1_000,
  };
}

function demoPreview(overrides: Partial<MfaImportPreview> = {}): MfaImportPreview {
  return normalizedPreview({
    sessionId: crypto.randomUUID(),
    name: "演示账户",
    issuer: "PetalDesk Demo",
    accountName: "demo@example.com",
    iconEmoji: "🌸",
    algorithm: "sha1",
    digits: 6,
    period: 30,
    warnings: ["浏览器预览仅使用模拟数据，不会保存你输入的密钥。"],
    ...overrides,
  });
}

/**
 * Creates the browser-only demonstration API. It deliberately keeps all state
 * in this object and never touches localStorage, IndexedDB, or a real TOTP secret.
 */
export function createBrowserMfaApi(): MfaApi {
  let entries: MfaEntrySummary[] = [
    {
      id: "browser-demo-petaldesk",
      name: "飞花演示",
      issuer: "PetalDesk",
      accountName: "demo@example.com",
      iconEmoji: "🌸",
      pinned: false,
      algorithm: "sha1",
      digits: 6,
      period: 30,
      createdAt: nowIso(),
      updatedAt: nowIso(),
    },
    {
      id: "browser-demo-github",
      name: "GitHub",
      issuer: "GitHub",
      accountName: "octocat",
      iconEmoji: "🐙",
      pinned: false,
      algorithm: "sha1",
      digits: 6,
      period: 30,
      createdAt: nowIso(),
      updatedAt: nowIso(),
    },
  ];
  let trash: MfaTrashEntrySummary[] = [];
  const previews = new Map<string, MfaImportPreview>();
  const batchEmojis = ["🔐", "🔑", "🛡️", "⭐", "☁️", "💼", "🏠", "🧰"];

  const remember = (preview: MfaImportPreview): MfaImportPreview[] => {
    previews.set(preview.sessionId, preview);
    return [structuredClone(preview)];
  };

  const previewFromText = (uri: string, iconEmoji?: string): MfaImportPreview[] => {
    let issuer = "PetalDesk Demo";
    let accountName = "demo@example.com";
    let name = "演示账户";
    try {
      const parsed = new URL(uri);
      if (parsed.protocol !== "otpauth:" || parsed.hostname.toLowerCase() !== "totp") {
        throw new Error("第一版只支持标准的 otpauth://totp 单账户链接。");
      }
      const label = decodeURIComponent(parsed.pathname.replace(/^\//, ""));
      const separator = label.indexOf(":");
      issuer = parsed.searchParams.get("issuer") || (separator >= 0 ? label.slice(0, separator) : issuer);
      accountName = separator >= 0 ? label.slice(separator + 1) : label || accountName;
      name = issuer || accountName || name;
    } catch (error) {
      if (error instanceof Error && error.message.includes("第一版")) throw error;
      throw new Error("请输入有效的 otpauth://totp 单账户链接。");
    }
    return remember(demoPreview({ name, issuer, accountName, iconEmoji }));
  };

  const commitPreviewBatch = (imports: MfaImportCommit[]): MfaEntrySummary[] => {
    if (imports.length === 0) throw new Error("没有可导入的账户。");
    if (new Set(imports.map((item) => item.sessionId)).size !== imports.length) {
      throw new Error("导入列表包含重复账户。");
    }
    const selected = imports.map((item) => {
      const preview = previews.get(item.sessionId);
      if (!preview) throw new Error("导入预览已经过期，请重新识别。");
      return { preview, iconEmoji: item.iconEmoji };
    });
    const timestamp = nowIso();
    const saved = selected.map(({ preview, iconEmoji }) => ({
      id: crypto.randomUUID(),
      name: preview.name,
      issuer: preview.issuer,
      accountName: preview.accountName,
      iconEmoji: iconEmoji || preview.iconEmoji || "🔐",
      pinned: false,
      algorithm: preview.algorithm,
      digits: preview.digits,
      period: preview.period,
      createdAt: timestamp,
      updatedAt: timestamp,
    } satisfies MfaEntrySummary));
    entries = [...entries, ...saved];
    for (const item of imports) previews.delete(item.sessionId);
    return structuredClone(saved);
  };

  return {
    isDesktop: () => false,
    async getStatus() {
      return {
        available: true,
        locked: false,
        entryCount: entries.length,
        protection: "browser-demo",
        recoveryState: "ready",
        captureExcluded: false,
        message: "浏览器预览使用模拟账户。",
      };
    },
    async list() {
      return structuredClone(entries);
    },
    async listTrash() {
      return structuredClone(trash);
    },
    async scanScreenQr() {
      return remember(demoPreview({ name: "屏幕扫码演示", iconEmoji: "📷" }));
    },
    async previewQrImage() {
      return remember(demoPreview({ name: "图片扫码演示", iconEmoji: "🖼️" }));
    },
    async previewUri(uri) {
      return previewFromText(uri);
    },
    async previewUris(uris) {
      const result: MfaUriPreviewResult = { previews: [], errors: [] };
      let emojiIndex = 0;
      for (const [index, rawLine] of uris.split(/\r?\n/).entries()) {
        const uri = rawLine.trim();
        if (!uri) continue;
        try {
          const [preview] = previewFromText(uri, batchEmojis[emojiIndex % batchEmojis.length]);
          emojiIndex += 1;
          if (preview) result.previews.push(preview);
        } catch (error) {
          result.errors.push({
            line: index + 1,
            message: errorMessage(error),
          });
        }
      }
      return result;
    },
    async previewManual(request) {
      if (!request.secret.trim()) throw new Error("请输入密钥。");
      return remember(demoPreview({
        name: request.name.trim() || request.issuer.trim() || request.accountName.trim() || "演示账户",
        issuer: request.issuer.trim(),
        accountName: request.accountName.trim(),
        iconEmoji: request.iconEmoji,
        algorithm: request.algorithm,
        digits: request.digits,
        period: request.period,
      }));
    },
    async commitImport(sessionId, iconEmoji) {
      return commitPreviewBatch([{ sessionId, iconEmoji }])[0];
    },
    async commitImports(imports) {
      return commitPreviewBatch(imports);
    },
    async cancelImport(sessionId) {
      previews.delete(sessionId);
    },
    async update(request) {
      const index = entries.findIndex((entry) => entry.id === request.id);
      if (index < 0) throw new Error("没有找到这个账户。");
      entries[index] = {
        ...entries[index],
        name: request.name.trim() || "未命名账户",
        issuer: request.issuer.trim(),
        accountName: request.accountName.trim(),
        iconEmoji: request.iconEmoji || "🔐",
        updatedAt: nowIso(),
      };
      return structuredClone(entries[index]);
    },
    async delete(id) {
      const deleted = entries.find((entry) => entry.id === id);
      if (!deleted) throw new Error("没有找到这个账户。");
      entries = entries.filter((entry) => entry.id !== id);
      trash = [{ ...deleted, deletedAt: nowIso() }, ...trash];
    },
    async restore(id) {
      const deleted = trash.find((entry) => entry.id === id);
      if (!deleted) throw new Error("回收站中没有找到这个账户。");
      const { deletedAt: _deletedAt, ...restored } = deleted;
      trash = trash.filter((entry) => entry.id !== id);
      entries = restored.pinned
        ? [restored, ...entries]
        : [...entries, restored];
      return structuredClone(restored);
    },
    async permanentlyDelete(id) {
      if (!trash.some((entry) => entry.id === id)) throw new Error("回收站中没有找到这个账户。");
      trash = trash.filter((entry) => entry.id !== id);
    },
    async emptyTrash() {
      trash = [];
    },
    async reorder(orderedIds) {
      if (orderedIds.length !== entries.length || new Set(orderedIds).size !== entries.length) {
        throw new Error("请提供完整且不重复的 MFA 账户顺序。");
      }
      const byId = new Map(entries.map((entry) => [entry.id, entry]));
      const reordered = orderedIds.flatMap((id) => {
        const entry = byId.get(id);
        return entry ? [entry] : [];
      });
      if (reordered.length !== entries.length) throw new Error("账户顺序中包含未知账户。");
      entries = pinnedFirst(reordered);
      return structuredClone(entries);
    },
    async setPinned(id, pinned) {
      const current = entries.find((entry) => entry.id === id);
      if (!current) throw new Error("没有找到这个账户。");
      const saved = { ...current, pinned, updatedAt: nowIso() };
      const remaining = entries.filter((entry) => entry.id !== id);
      entries = pinned
        ? [saved, ...remaining.filter((entry) => entry.pinned), ...remaining.filter((entry) => !entry.pinned)]
        : [...remaining.filter((entry) => entry.pinned), saved, ...remaining.filter((entry) => !entry.pinned)];
      return structuredClone(entries);
    },
    async reveal(id) {
      const entry = entries.find((item) => item.id === id);
      if (!entry) throw new Error("没有找到这个账户。");
      return demoCode(entry.id, entry.digits, entry.period);
    },
    async exportEntry() {
      throw new Error("浏览器预览不包含可导出的真实密钥，请在桌面客户端使用。");
    },
    async copy(id) {
      const entry = entries.find((item) => item.id === id);
      if (!entry) throw new Error("没有找到这个账户。");
      const code = demoCode(entry.id, entry.digits, entry.period).code;
      if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(code);
      }
    },
    async configureRecoveryPassword() {},
    async unlockWithRecoveryPassword() {},
    async lock() {
      previews.clear();
    },
  };
}

const browserApi = createBrowserMfaApi();

export const mfaApi: MfaApi = {
  isDesktop: isDesktopRuntime,

  async getStatus() {
    if (!isDesktopRuntime()) return browserApi.getStatus();
    return command<MfaStatus>("get_mfa_status");
  },

  async list() {
    if (!isDesktopRuntime()) return browserApi.list();
    return command<MfaEntrySummary[]>("list_mfa_entries");
  },

  async listTrash() {
    if (!isDesktopRuntime()) return browserApi.listTrash();
    return command<MfaTrashEntrySummary[]>("list_mfa_trash");
  },

  async scanScreenQr() {
    if (!isDesktopRuntime()) return browserApi.scanScreenQr();
    return previewList(await command<MfaImportPreview | MfaImportPreview[]>("scan_mfa_screen_qr"));
  },

  async previewQrImage(bytes, mediaType = "application/octet-stream") {
    if (!isDesktopRuntime()) return browserApi.previewQrImage(bytes, mediaType);
    try {
      const result = await invoke<MfaImportPreview | MfaImportPreview[]>("preview_mfa_qr_image", bytes, {
        headers: { "content-type": mediaType || "application/octet-stream" },
      });
      return previewList(result);
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  },

  async previewUri(uri) {
    if (!isDesktopRuntime()) return browserApi.previewUri(uri);
    return previewList(await command<MfaImportPreview | MfaImportPreview[]>("preview_mfa_uri", { uri }));
  },

  async previewUris(uris) {
    if (!isDesktopRuntime()) return browserApi.previewUris(uris);
    return normalizedUriPreviewResult(await command<MfaUriPreviewResult>("preview_mfa_uris", { uris }));
  },

  async previewManual(request) {
    if (!isDesktopRuntime()) return browserApi.previewManual(request);
    return previewList(await command<MfaImportPreview | MfaImportPreview[]>("preview_mfa_manual", { request }));
  },

  async commitImport(sessionId, iconEmoji) {
    if (!isDesktopRuntime()) return browserApi.commitImport(sessionId, iconEmoji);
    return command<MfaEntrySummary>("commit_mfa_import", { sessionId, iconEmoji });
  },

  async commitImports(imports) {
    if (!isDesktopRuntime()) return browserApi.commitImports(imports);
    return command<MfaEntrySummary[]>("commit_mfa_imports", { imports });
  },

  async cancelImport(sessionId) {
    if (!isDesktopRuntime()) return browserApi.cancelImport(sessionId);
    await command<void>("cancel_mfa_import", { sessionId });
  },

  async update(request) {
    if (!isDesktopRuntime()) return browserApi.update(request);
    return command<MfaEntrySummary>("update_mfa_entry", { request });
  },

  async delete(id) {
    if (!isDesktopRuntime()) return browserApi.delete(id);
    await command<void>("delete_mfa_entry", { entryId: id });
  },

  async restore(id) {
    if (!isDesktopRuntime()) return browserApi.restore(id);
    return command<MfaEntrySummary>("restore_mfa_entry", { entryId: id });
  },

  async permanentlyDelete(id) {
    if (!isDesktopRuntime()) return browserApi.permanentlyDelete(id);
    await command<void>("permanently_delete_mfa_entry", { entryId: id });
  },

  async emptyTrash() {
    if (!isDesktopRuntime()) return browserApi.emptyTrash();
    await command<void>("empty_mfa_trash");
  },

  async reorder(orderedIds) {
    if (!isDesktopRuntime()) return browserApi.reorder(orderedIds);
    return command<MfaEntrySummary[]>("reorder_mfa_entries", { orderedIds });
  },

  async setPinned(id, pinned) {
    if (!isDesktopRuntime()) return browserApi.setPinned(id, pinned);
    return command<MfaEntrySummary[]>("set_mfa_entry_pinned", { entryId: id, pinned });
  },

  async reveal(id) {
    if (!isDesktopRuntime()) return browserApi.reveal(id);
    return command<MfaRevealResult>("reveal_mfa_code", { entryId: id });
  },

  async exportEntry(id, password) {
    if (!isDesktopRuntime()) return browserApi.exportEntry(id, password);
    return command<MfaEntryExport>("export_mfa_entry", { entryId: id, password });
  },

  async copy(id) {
    if (!isDesktopRuntime()) return browserApi.copy(id);
    await command<void>("copy_mfa_code", { entryId: id });
  },

  async configureRecoveryPassword(password, currentPassword) {
    if (!isDesktopRuntime()) return browserApi.configureRecoveryPassword(password, currentPassword);
    const args = currentPassword === undefined ? { password } : { password, currentPassword };
    await command<void>("configure_mfa_recovery_password", args);
  },

  async unlockWithRecoveryPassword(password) {
    if (!isDesktopRuntime()) return browserApi.unlockWithRecoveryPassword(password);
    await command<void>("unlock_mfa_with_recovery_password", { password });
  },

  async lock() {
    if (!isDesktopRuntime()) return browserApi.lock();
    await command<void>("lock_mfa_vault");
  },
};
