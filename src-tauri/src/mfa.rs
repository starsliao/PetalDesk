//! Local, Windows-user-bound MFA (TOTP) vault.
//!
//! This module deliberately keeps the account secret out of all public
//! serialised structures.  Only a short-lived reveal operation returns a
//! generated code to the webview.  The on-disk vault contains an
//! XChaCha20-Poly1305 envelope whose random key is protected by the current
//! Windows user's DPAPI.

use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write, atomic_write_json, INTERNAL_DATA_DIR};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{SecondsFormat, Utc};
use data_encoding::BASE32_NOPAD;
use image::{ImageReader, Limits};
use quircs::Quirc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use totp_rs::{Algorithm as TotpAlgorithm, TOTP};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const MFA_DIR: &str = "mfa";
const VAULT_FILE: &str = "vault.json";
const SETTINGS_FILE: &str = "settings.json";
const BACKUP_DIR: &str = "backups";
const VAULT_SCHEMA_VERSION: u32 = 1;
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const VAULT_AAD: &[u8] = b"PetalDesk MFA vault v1";
const MAX_VAULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_VAULT_ENTRIES: usize = 10_000;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_WIDTH: u32 = 12_000;
const MAX_IMAGE_HEIGHT: u32 = 12_000;
const MAX_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;
const MAX_QR_SESSIONS: usize = 32;
const IMPORT_SESSION_TTL: Duration = Duration::from_secs(5 * 60);
const CLIPBOARD_MAX_SECONDS: Duration = Duration::from_secs(30);
const CLIPBOARD_RETRY_COUNT: usize = 10;
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(20);
const CLIPBOARD_CLEANUP_TIMEOUT: Duration = Duration::from_secs(35);

fn generic_vault_error() -> AppError {
    AppError::new(
        "mfa_vault_unavailable",
        "MFA 数据保险库无法解锁，请确认当前 Windows 用户未发生变化。",
    )
}

fn generic_qr_error() -> AppError {
    AppError::new("mfa_qr_invalid", "没有识别到有效的标准 TOTP 二维码。")
}

fn mfa_session_closed_error() -> AppError {
    AppError::new(
        "mfa_session_closed",
        "MFA 验证器已关闭，请重新打开后再操作。",
    )
}

/// A deserialisable but non-debuggable string used for URI/manual import
/// inputs.  Its contents are wiped when the value leaves scope.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct SensitiveText(String);

impl SensitiveText {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SensitiveText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveText(<redacted>)")
    }
}

impl Drop for SensitiveText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MfaAlgorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl Default for MfaAlgorithm {
    fn default() -> Self {
        Self::Sha1
    }
}

impl MfaAlgorithm {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "SHA1" | "SHA-1" => Some(Self::Sha1),
            "SHA256" | "SHA-256" => Some(Self::Sha256),
            "SHA512" | "SHA-512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn totp(self) -> TotpAlgorithm {
        match self {
            Self::Sha1 => TotpAlgorithm::SHA1,
            Self::Sha256 => TotpAlgorithm::SHA256,
            Self::Sha512 => TotpAlgorithm::SHA512,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaStatus {
    pub available: bool,
    pub locked: bool,
    pub entry_count: usize,
    pub protection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_excluded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaEntrySummary {
    pub id: String,
    pub name: String,
    pub issuer: String,
    pub account_name: String,
    pub icon_emoji: String,
    pub algorithm: MfaAlgorithm,
    pub digits: u32,
    pub period: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaRevealResult {
    pub id: String,
    pub code: String,
    pub valid_until: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaImportPreview {
    pub session_id: String,
    pub name: String,
    pub issuer: String,
    pub account_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_emoji: Option<String>,
    pub algorithm: MfaAlgorithm,
    pub digits: u32,
    pub period: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaManualImportRequest {
    pub name: String,
    pub issuer: String,
    pub account_name: String,
    pub secret: SensitiveText,
    pub icon_emoji: String,
    pub algorithm: MfaAlgorithm,
    pub digits: u32,
    pub period: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaEntryUpdateRequest {
    pub id: String,
    pub name: String,
    pub issuer: String,
    pub account_name: String,
    pub icon_emoji: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MfaSettings {
    #[serde(default = "settings_schema_version")]
    schema_version: u32,
}

fn settings_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

impl Default for MfaSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultEnvelope {
    schema_version: u32,
    wrapped_key: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultPayload {
    schema_version: u32,
    entries: Vec<StoredEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEntry {
    id: String,
    name: String,
    issuer: String,
    account_name: String,
    icon_emoji: String,
    algorithm: MfaAlgorithm,
    digits: u32,
    period: u64,
    created_at: String,
    updated_at: String,
    secret: Vec<u8>,
}

impl Drop for StoredEntry {
    fn drop(&mut self) {
        self.secret.zeroize();
        self.name.zeroize();
        self.issuer.zeroize();
        self.account_name.zeroize();
        self.icon_emoji.zeroize();
    }
}

struct UnlockedVault {
    payload: VaultPayload,
    key: Zeroizing<Vec<u8>>,
    wrapped_key: Vec<u8>,
}

impl Drop for UnlockedVault {
    fn drop(&mut self) {
        self.wrapped_key.zeroize();
    }
}

struct PendingImport {
    entry: StoredEntry,
    expires_at: Instant,
}

struct ClipboardLease {
    sequence: u32,
    marker: Vec<u8>,
    clear_at: Instant,
}

struct RuntimeState {
    vault: Option<UnlockedVault>,
    unavailable: bool,
    recovered_from_backup: bool,
}

pub struct MfaStore {
    vault_path: PathBuf,
    backup_path: PathBuf,
    runtime: Mutex<RuntimeState>,
    imports: Arc<Mutex<HashMap<String, PendingImport>>>,
    session_epoch: AtomicU64,
    session_active: AtomicBool,
    lifecycle_lock: Mutex<()>,
    clipboard: Arc<Mutex<Option<ClipboardLease>>>,
    capture_excluded: AtomicU8,
}

impl MfaStore {
    pub fn load(data_storage_path: &Path) -> AppResult<Self> {
        let root = data_storage_path
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join(MFA_DIR);
        std::fs::create_dir_all(&root).map_err(|e| AppError::io("创建 MFA 数据目录", e))?;
        let backup_path = root.join(BACKUP_DIR);
        std::fs::create_dir_all(&backup_path).map_err(|e| AppError::io("创建 MFA 备份目录", e))?;
        let settings_path = root.join(SETTINGS_FILE);
        if !settings_path.exists() {
            atomic_write_json(&settings_path, &MfaSettings::default())?;
        } else {
            let settings_are_valid = std::fs::metadata(&settings_path)
                .ok()
                .filter(|metadata| metadata.len() <= MAX_SETTINGS_BYTES)
                .and_then(|_| std::fs::read(&settings_path).ok())
                .and_then(|bytes| serde_json::from_slice::<MfaSettings>(&bytes).ok())
                .is_some_and(|settings| settings.schema_version == SETTINGS_SCHEMA_VERSION);
            if !settings_are_valid {
                preserve_corrupt_file(&settings_path);
                atomic_write_json(&settings_path, &MfaSettings::default())?;
            }
        }
        let vault_path = root.join(VAULT_FILE);
        // Validate only the envelope shape here. DPAPI is intentionally lazy:
        // copying the vault to another Windows user must not prevent the app
        // itself from starting.
        let unavailable = if vault_path.exists() {
            read_envelope(&vault_path).is_err()
        } else {
            false
        };
        let imports = Arc::new(Mutex::new(HashMap::new()));
        let imports_for_cleanup = Arc::downgrade(&imports);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let Some(imports) = imports_for_cleanup.upgrade() else {
                break;
            };
            purge_expired_imports(&mut lock_unpoisoned(&imports));
        });
        Ok(Self {
            vault_path,
            backup_path,
            runtime: Mutex::new(RuntimeState {
                vault: None,
                unavailable,
                recovered_from_backup: false,
            }),
            imports,
            session_epoch: AtomicU64::new(0),
            session_active: AtomicBool::new(false),
            lifecycle_lock: Mutex::new(()),
            clipboard: Arc::new(Mutex::new(None)),
            capture_excluded: AtomicU8::new(0),
        })
    }

    /// Marks an MFA window session active. Refocusing the same live window is
    /// intentionally idempotent so it cannot invalidate its own in-flight
    /// reveal or import command.
    pub fn activate(&self) -> u64 {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        if self.session_active.load(Ordering::Acquire) {
            return self.session_epoch.load(Ordering::Acquire);
        }
        let epoch = self.session_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.session_active.store(true, Ordering::Release);
        epoch
    }

    /// Atomically invalidates queued work before any potentially blocking
    /// mutex/DPAPI cleanup begins.
    pub fn deactivate(&self) -> u64 {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.session_active.store(false, Ordering::Release);
        self.session_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn require_active_epoch(&self) -> AppResult<u64> {
        if !self.session_active.load(Ordering::Acquire) {
            return Err(mfa_session_closed_error());
        }
        let epoch = self.session_epoch.load(Ordering::Acquire);
        if !self.session_active.load(Ordering::Acquire)
            || self.session_epoch.load(Ordering::Acquire) != epoch
        {
            return Err(mfa_session_closed_error());
        }
        Ok(epoch)
    }

    fn validate_epoch(&self, epoch: u64) -> AppResult<()> {
        if self.session_active.load(Ordering::Acquire)
            && self.session_epoch.load(Ordering::Acquire) == epoch
        {
            Ok(())
        } else {
            Err(mfa_session_closed_error())
        }
    }

    fn ensure_unlocked_at(&self, runtime: &mut RuntimeState, epoch: u64) -> AppResult<()> {
        self.validate_epoch(epoch)?;
        let result = self.ensure_unlocked(runtime);
        if let Err(error) = self.validate_epoch(epoch) {
            runtime.vault = None;
            return Err(error);
        }
        result
    }

    #[cfg(test)]
    pub fn status(&self) -> AppResult<MfaStatus> {
        let epoch = self.require_active_epoch()?;
        self.status_at(epoch)
    }

    fn status_at(&self, epoch: u64) -> AppResult<MfaStatus> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        let available = self.ensure_unlocked_at(&mut runtime, epoch).is_ok();
        let (locked, entry_count) = runtime
            .vault
            .as_ref()
            .map(|vault| (false, vault.payload.entries.len()))
            .unwrap_or((self.vault_path.exists(), 0));
        Ok(MfaStatus {
            available,
            locked,
            entry_count,
            protection: if cfg!(windows) {
                "windows-dpapi".to_string()
            } else {
                "unsupported".to_string()
            },
            capture_excluded: match self.capture_excluded.load(Ordering::Acquire) {
                1 => Some(false),
                2 => Some(true),
                _ => None,
            },
            message: if available {
                None
            } else {
                Some("MFA 数据当前无法解锁；不会创建空白保险库。".to_string())
            },
        })
    }

    #[cfg(test)]
    pub fn list_entries(&self) -> AppResult<Vec<MfaEntrySummary>> {
        let epoch = self.require_active_epoch()?;
        self.list_entries_at(epoch)
    }

    fn list_entries_at(&self, epoch: u64) -> AppResult<Vec<MfaEntrySummary>> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        Ok(vault.payload.entries.iter().map(summary).collect())
    }

    pub fn set_capture_excluded(&self, excluded: bool) {
        self.capture_excluded
            .store(if excluded { 2 } else { 1 }, Ordering::Release);
    }

    pub fn preview_uri(&self, uri: &str) -> AppResult<Vec<MfaImportPreview>> {
        let epoch = self.require_active_epoch()?;
        let parsed = parse_otpauth_uri(uri)?;
        self.remember_imports(vec![parsed], epoch)
    }

    pub fn preview_manual(
        &self,
        request: MfaManualImportRequest,
    ) -> AppResult<Vec<MfaImportPreview>> {
        let epoch = self.require_active_epoch()?;
        let parsed = parse_manual(request)?;
        self.remember_imports(vec![parsed], epoch)
    }

    fn preview_image_at(&self, bytes: &[u8], epoch: u64) -> AppResult<Vec<MfaImportPreview>> {
        self.validate_epoch(epoch)?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(AppError::invalid("二维码图片过大，请选择较小的图片。"));
        }
        let image = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| generic_qr_error())?;
        let mut image = image;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_WIDTH);
        limits.max_image_height = Some(MAX_IMAGE_HEIGHT);
        limits.max_alloc = Some(MAX_IMAGE_ALLOC);
        image.limits(limits);
        let mut gray = image.decode().map_err(|_| generic_qr_error())?.to_luma8();
        let result = self.preview_luma_at_epoch(gray.width(), gray.height(), gray.as_raw(), epoch);
        gray.as_mut().fill(0);
        result
    }

    fn preview_luma_at_epoch(
        &self,
        width: u32,
        height: u32,
        gray: &[u8],
        epoch: u64,
    ) -> AppResult<Vec<MfaImportPreview>> {
        let payloads = decode_qr_payloads(width, height, gray)?;
        let mut pending = Vec::new();
        let mut saw_migration = false;
        for mut payload in payloads {
            let text = match std::str::from_utf8(&payload) {
                Ok(value) => value,
                Err(_) => {
                    payload.zeroize();
                    continue;
                }
            };
            if starts_with_ignore_ascii_case(text, "otpauth-migration://") {
                saw_migration = true;
                payload.zeroize();
                continue;
            }
            if starts_with_ignore_ascii_case(text, "otpauth://totp/") {
                if let Ok(import) = parse_otpauth_uri(text) {
                    pending.push(import);
                }
            }
            payload.zeroize();
        }
        if pending.is_empty() {
            if saw_migration {
                return Err(AppError::new(
                    "mfa_migration_unsupported",
                    "第一版只支持标准单账户 TOTP 二维码，不支持 Google 批量迁移二维码。",
                ));
            }
            return Err(generic_qr_error());
        }
        self.remember_imports(pending, epoch)
    }

    #[cfg(test)]
    pub fn commit_import(&self, session_id: &str, icon_emoji: &str) -> AppResult<MfaEntrySummary> {
        let epoch = self.require_active_epoch()?;
        self.commit_import_at(session_id, icon_emoji, epoch)
    }

    fn commit_import_at(
        &self,
        session_id: &str,
        icon_emoji: &str,
        epoch: u64,
    ) -> AppResult<MfaEntrySummary> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let pending = {
            let mut imports = lock_unpoisoned(&self.imports);
            purge_expired_imports(&mut imports);
            imports
                .remove(session_id)
                .ok_or_else(|| AppError::not_found("导入预览已经过期，请重新识别。"))?
        };
        let mut entry = pending.entry;
        entry.icon_emoji = normalize_icon(icon_emoji);
        entry.updated_at = now_iso();
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        if vault.payload.entries.len() >= MAX_VAULT_ENTRIES {
            return Err(AppError::new(
                "mfa_vault_entry_limit",
                "MFA 账户数量已达到安全上限。",
            ));
        }
        let summary = summary(&entry);
        vault.payload.entries.push(entry);
        if let Err(error) = self.save_vault(vault) {
            let _ = vault.payload.entries.pop();
            return Err(error);
        }
        Ok(summary)
    }

    pub fn cancel_import(&self, session_id: &str) -> AppResult<()> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.require_active_epoch()?;
        let mut imports = lock_unpoisoned(&self.imports);
        imports.remove(session_id);
        Ok(())
    }

    #[cfg(test)]
    pub fn update_entry(&self, request: MfaEntryUpdateRequest) -> AppResult<MfaEntrySummary> {
        let epoch = self.require_active_epoch()?;
        self.update_entry_at(request, epoch)
    }

    fn update_entry_at(
        &self,
        request: MfaEntryUpdateRequest,
        epoch: u64,
    ) -> AppResult<MfaEntrySummary> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        // Validate every field before touching the unlocked in-memory entry so
        // an invalid later field cannot leave an earlier field half-updated.
        let issuer = normalize_label_text(&request.issuer, 256, "发行方")?;
        let account_name = normalize_label_text(&request.account_name, 256, "账户")?;
        let mut name = normalize_label_text(&request.name, 120, "账户名称")?;
        if name.is_empty() {
            name = if !issuer.is_empty() {
                issuer.clone()
            } else if !account_name.is_empty() {
                account_name.clone()
            } else {
                "未命名账户".to_string()
            };
        }
        let icon_emoji = normalize_icon(&request.icon_emoji);
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let index = vault
            .payload
            .entries
            .iter()
            .position(|entry| entry.id == request.id)
            .ok_or_else(|| AppError::not_found("没有找到这个 MFA 账户。"))?;
        let old = {
            let entry = &vault.payload.entries[index];
            (
                entry.name.clone(),
                entry.issuer.clone(),
                entry.account_name.clone(),
                entry.icon_emoji.clone(),
                entry.updated_at.clone(),
            )
        };
        let entry = &mut vault.payload.entries[index];
        entry.name = name;
        entry.issuer = issuer;
        entry.account_name = account_name;
        entry.icon_emoji = icon_emoji;
        entry.updated_at = now_iso();
        let result = summary(entry);
        if let Err(error) = self.save_vault(vault) {
            let entry = &mut vault.payload.entries[index];
            entry.name = old.0;
            entry.issuer = old.1;
            entry.account_name = old.2;
            entry.icon_emoji = old.3;
            entry.updated_at = old.4;
            return Err(error);
        }
        Ok(result)
    }

    #[cfg(test)]
    pub fn delete_entry(&self, entry_id: &str) -> AppResult<()> {
        let epoch = self.require_active_epoch()?;
        self.delete_entry_at(entry_id, epoch)
    }

    fn delete_entry_at(&self, entry_id: &str, epoch: u64) -> AppResult<()> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let index = vault
            .payload
            .entries
            .iter()
            .position(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个 MFA 账户。"))?;
        let removed = vault.payload.entries.remove(index);
        if let Err(error) = self.save_vault(vault) {
            vault.payload.entries.insert(index, removed);
            return Err(error);
        }
        Ok(())
    }

    pub fn reveal_code(&self, entry_id: &str) -> AppResult<MfaRevealResult> {
        let epoch = self.require_active_epoch()?;
        self.reveal_code_at(entry_id, epoch)
    }

    fn reveal_code_at(&self, entry_id: &str, epoch: u64) -> AppResult<MfaRevealResult> {
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        let entry = vault
            .payload
            .entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个 MFA 账户。"))?;
        let (code, valid_until) = generate_code(entry, unix_seconds());
        Ok(MfaRevealResult {
            id: entry.id.clone(),
            code,
            valid_until: valid_until.saturating_mul(1_000),
        })
    }

    fn copy_entry_code_at(&self, entry_id: &str, epoch: u64) -> AppResult<()> {
        self.validate_epoch(epoch)?;
        // Do not copy a code with only a few seconds left: wait for the next
        // period, then give the user a full usable interval.
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        let entry = vault
            .payload
            .entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个 MFA 账户。"))?;
        let now = unix_seconds();
        let (_, next) = generate_code(entry, now);
        let remaining = next.saturating_sub(now);
        if remaining <= 3 {
            drop(runtime);
            std::thread::sleep(Duration::from_secs(remaining + 1));
            runtime = lock_unpoisoned(&self.runtime);
            self.ensure_unlocked_at(&mut runtime, epoch)?;
        }
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        let entry = vault
            .payload
            .entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个 MFA 账户。"))?;
        let now = unix_seconds();
        let (code, valid_until) = generate_code(entry, now);
        let code = Zeroizing::new(code);
        drop(runtime);
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        write_code_to_clipboard(&code, valid_until, &self.clipboard)?;
        Ok(())
    }

    /// Clears decrypted entries, pending imports and an unchanged MFA
    /// clipboard value.  This is safe to call repeatedly from window destroy
    /// and application exit handlers.
    pub fn lock(&self) {
        let epoch = self.deactivate();
        self.clear_deactivated_state(epoch);
    }

    pub fn clear_deactivated_state(&self, epoch: u64) {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        if self.session_active.load(Ordering::Acquire)
            || self.session_epoch.load(Ordering::Acquire) != epoch
        {
            return;
        }
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.vault = None;
        let mut imports = lock_unpoisoned(&self.imports);
        imports.clear();
        force_expire_clipboard(&self.clipboard);
        if !clear_clipboard_now(&self.clipboard) {
            // Keep a background retry for a clipboard owner that is briefly
            // busy, while the synchronous attempts above cover explicit app
            // exit before Windows tears down the process.
            schedule_clipboard_cleanup(self.clipboard.clone(), Instant::now());
        }
    }

    fn remember_imports(
        &self,
        imports: Vec<ParsedImport>,
        epoch: u64,
    ) -> AppResult<Vec<MfaImportPreview>> {
        let imports = imports
            .into_iter()
            .take(MAX_QR_SESSIONS)
            .collect::<Vec<_>>();
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut sessions = lock_unpoisoned(&self.imports);
        purge_expired_imports(&mut sessions);
        while sessions.len() + imports.len() > MAX_QR_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, value)| value.expires_at)
                .map(|(id, _)| id.clone())
            {
                sessions.remove(&oldest);
            } else {
                break;
            }
        }
        let mut previews = Vec::with_capacity(imports.len());
        for parsed in imports {
            let session_id = Uuid::new_v4().to_string();
            let preview = MfaImportPreview {
                session_id: session_id.clone(),
                name: parsed.entry.name.clone(),
                issuer: parsed.entry.issuer.clone(),
                account_name: parsed.entry.account_name.clone(),
                icon_emoji: Some(parsed.entry.icon_emoji.clone()),
                algorithm: parsed.entry.algorithm,
                digits: parsed.entry.digits,
                period: parsed.entry.period,
                warnings: parsed.warnings.clone(),
            };
            sessions.insert(
                session_id,
                PendingImport {
                    entry: parsed.entry,
                    expires_at: Instant::now() + IMPORT_SESSION_TTL,
                },
            );
            previews.push(preview);
        }
        Ok(previews)
    }

    fn ensure_unlocked(&self, runtime: &mut RuntimeState) -> AppResult<()> {
        if runtime.vault.is_some() {
            return Ok(());
        }
        if !self.vault_path.exists() {
            runtime.unavailable = false;
            runtime.vault = Some(new_empty_vault()?);
            return Ok(());
        }
        let primary_len = std::fs::metadata(&self.vault_path)
            .map_err(|_| generic_vault_error())?
            .len();
        if primary_len > MAX_VAULT_BYTES as u64 {
            if let Some((bytes, vault)) = self.find_valid_backup() {
                preserve_corrupt_path(&self.vault_path)?;
                atomic_write(&self.vault_path, &bytes)?;
                runtime.unavailable = false;
                runtime.recovered_from_backup = true;
                runtime.vault = Some(vault);
                return Ok(());
            }
            runtime.unavailable = true;
            return Err(generic_vault_error());
        }
        let primary = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
        match decrypt_envelope(&primary) {
            Ok(vault) => {
                runtime.unavailable = false;
                runtime.vault = Some(vault);
                Ok(())
            }
            Err(_) => {
                // Preserve the damaged primary and try encrypted backups.  A
                // backup is accepted only after DPAPI + AEAD authentication.
                if let Some((bytes, vault)) = self.find_valid_backup() {
                    // Copy the damaged bytes first; never rename the only
                    // primary away before a replacement has been durably
                    // written. `atomic_write` leaves the destination intact
                    // if replacement fails.
                    preserve_corrupt_bytes(&self.vault_path, &primary)?;
                    atomic_write(&self.vault_path, &bytes)?;
                    runtime.unavailable = false;
                    runtime.recovered_from_backup = true;
                    runtime.vault = Some(vault);
                    Ok(())
                } else {
                    runtime.unavailable = true;
                    Err(generic_vault_error())
                }
            }
        }
    }

    fn find_valid_backup(&self) -> Option<(Vec<u8>, UnlockedVault)> {
        let mut files = std::fs::read_dir(&self.backup_path)
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        for file in files {
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if metadata.len() > MAX_VAULT_BYTES as u64 {
                continue;
            }
            let Ok(bytes) = std::fs::read(file.path()) else {
                continue;
            };
            if let Ok(vault) = decrypt_envelope(&bytes) {
                return Some((bytes, vault));
            }
        }
        None
    }

    fn save_vault(&self, vault: &mut UnlockedVault) -> AppResult<()> {
        let mut plaintext =
            Zeroizing::new(serde_json::to_vec(&vault.payload).map_err(|_| generic_vault_error())?);
        if plaintext.len() > MAX_VAULT_BYTES {
            return Err(AppError::new(
                "mfa_vault_too_large",
                "MFA 数据保险库超过安全大小限制。",
            ));
        }
        let mut nonce_bytes = [0u8; 24];
        getrandom::fill(&mut nonce_bytes).map_err(|_| generic_vault_error())?;
        let nonce: XNonce = nonce_bytes.into();
        let cipher =
            XChaCha20Poly1305::new_from_slice(&vault.key).map_err(|_| generic_vault_error())?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plaintext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| generic_vault_error())?;
        plaintext.zeroize();
        let envelope = VaultEnvelope {
            schema_version: VAULT_SCHEMA_VERSION,
            wrapped_key: STANDARD_NO_PAD.encode(&vault.wrapped_key),
            nonce: STANDARD_NO_PAD.encode(nonce_bytes),
            ciphertext: STANDARD_NO_PAD.encode(ciphertext),
        };
        let bytes = serde_json::to_vec_pretty(&envelope).map_err(|_| generic_vault_error())?;
        if bytes.len() > MAX_VAULT_BYTES {
            return Err(AppError::new(
                "mfa_vault_too_large",
                "MFA 数据保险库超过安全大小限制。",
            ));
        }
        self.rotate_backup()?;
        atomic_write(&self.vault_path, &bytes)
    }

    fn rotate_backup(&self) -> AppResult<()> {
        if !self.vault_path.exists() {
            return Ok(());
        }
        let previous_len = std::fs::metadata(&self.vault_path)
            .map_err(|_| generic_vault_error())?
            .len();
        if previous_len > MAX_VAULT_BYTES as u64 {
            return Err(generic_vault_error());
        }
        let previous = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
        let file = self.backup_path.join(format!(
            "vault-{}-{}.json",
            Utc::now().format("%Y%m%d%H%M%S%3f"),
            Uuid::new_v4()
        ));
        atomic_write(&file, &previous)?;
        let mut files = std::fs::read_dir(&self.backup_path)
            .map_err(|_| generic_vault_error())?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        for old in files.into_iter().skip(5) {
            let _ = std::fs::remove_file(old.path());
        }
        Ok(())
    }
}

fn new_empty_vault() -> AppResult<UnlockedVault> {
    let mut key = Zeroizing::new(vec![0u8; 32]);
    getrandom::fill(&mut key).map_err(|_| generic_vault_error())?;
    let wrapped_key = protect_key(&key)?;
    Ok(UnlockedVault {
        payload: VaultPayload {
            schema_version: VAULT_SCHEMA_VERSION,
            entries: Vec::new(),
        },
        key,
        wrapped_key,
    })
}

fn read_envelope(path: &Path) -> AppResult<VaultEnvelope> {
    let length = std::fs::metadata(path)
        .map_err(|_| generic_vault_error())?
        .len();
    if length > MAX_VAULT_BYTES as u64 {
        return Err(generic_vault_error());
    }
    let bytes = std::fs::read(path).map_err(|_| generic_vault_error())?;
    if bytes.len() > MAX_VAULT_BYTES {
        return Err(generic_vault_error());
    }
    serde_json::from_slice(&bytes).map_err(|_| generic_vault_error())
}

fn decrypt_envelope(bytes: &[u8]) -> AppResult<UnlockedVault> {
    let envelope: VaultEnvelope =
        serde_json::from_slice(bytes).map_err(|_| generic_vault_error())?;
    if envelope.schema_version != VAULT_SCHEMA_VERSION {
        return Err(generic_vault_error());
    }
    let wrapped = STANDARD_NO_PAD
        .decode(envelope.wrapped_key.as_bytes())
        .map_err(|_| generic_vault_error())?;
    let nonce_bytes = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|_| generic_vault_error())?;
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| generic_vault_error())?;
    if nonce_bytes.len() != 24
        || wrapped.is_empty()
        || wrapped.len() > 4096
        || ciphertext.is_empty()
    {
        return Err(generic_vault_error());
    }
    let key = unprotect_key(&wrapped)?;
    if key.len() != 32 {
        return Err(generic_vault_error());
    }
    let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| generic_vault_error())?;
    let nonce_array: [u8; 24] = nonce_bytes
        .as_slice()
        .try_into()
        .map_err(|_| generic_vault_error())?;
    let nonce: XNonce = nonce_array.into();
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| generic_vault_error())?,
    );
    let payload: VaultPayload =
        serde_json::from_slice(&plaintext).map_err(|_| generic_vault_error())?;
    if payload.schema_version != VAULT_SCHEMA_VERSION || payload.entries.len() > MAX_VAULT_ENTRIES {
        return Err(generic_vault_error());
    }
    for entry in &payload.entries {
        validate_stored_entry(entry)?;
    }
    Ok(UnlockedVault {
        payload,
        key,
        wrapped_key: wrapped,
    })
}

fn validate_stored_entry(entry: &StoredEntry) -> AppResult<()> {
    if entry.id.is_empty()
        || entry.id.len() > 128
        || entry.digits < 6
        || entry.digits > 8
        || entry.period == 0
        || entry.period > 3_600
        || entry.secret.len() < 10
        || entry.secret.len() > 128
    {
        return Err(generic_vault_error());
    }
    Ok(())
}

fn preserve_corrupt_file(path: &Path) {
    if !path.exists() {
        return;
    }
    let suffix = Utc::now().format("%Y%m%d%H%M%S%3f");
    let target = path.with_file_name(format!(
        "{}.corrupt-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ));
    let _ = std::fs::rename(path, target);
}

fn preserve_corrupt_bytes(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let suffix = Utc::now().format("%Y%m%d%H%M%S%3f");
    let target = path.with_file_name(format!(
        "{}.corrupt-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ));
    atomic_write(&target, bytes)
}

fn preserve_corrupt_path(path: &Path) -> AppResult<()> {
    let suffix = Utc::now().format("%Y%m%d%H%M%S%3f");
    let target = path.with_file_name(format!(
        "{}.corrupt-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ));
    std::fs::copy(path, target)
        .map(|_| ())
        .map_err(|error| AppError::io("保留损坏的 MFA 保险库", error))
}

fn summary(entry: &StoredEntry) -> MfaEntrySummary {
    MfaEntrySummary {
        id: entry.id.clone(),
        name: entry.name.clone(),
        issuer: entry.issuer.clone(),
        account_name: entry.account_name.clone(),
        icon_emoji: entry.icon_emoji.clone(),
        algorithm: entry.algorithm,
        digits: entry.digits,
        period: entry.period,
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn generate_code(entry: &StoredEntry, now: u64) -> (String, u64) {
    let totp = TOTP::new_unchecked(
        entry.algorithm.totp(),
        entry.digits as usize,
        0,
        entry.period,
        entry.secret.clone(),
    );
    let code = totp.generate(now);
    let valid_until = totp.next_step(now);
    (code, valid_until)
}

struct ParsedImport {
    entry: StoredEntry,
    warnings: Vec<String>,
}

struct ParsedEntryInput {
    name: String,
    issuer: String,
    account: String,
    icon: String,
    algorithm: MfaAlgorithm,
    digits: u32,
    period: u64,
    secret: Vec<u8>,
}

impl std::fmt::Debug for ParsedImport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ParsedImport")
            .field("entry", &"<redacted>")
            .field("warnings", &self.warnings)
            .finish()
    }
}

fn parse_manual(request: MfaManualImportRequest) -> AppResult<ParsedImport> {
    let secret = decode_base32(request.secret.as_str())?;
    parse_entry(ParsedEntryInput {
        name: request.name,
        issuer: request.issuer,
        account: request.account_name,
        icon: request.icon_emoji,
        algorithm: request.algorithm,
        digits: request.digits,
        period: request.period,
        secret,
    })
}

fn parse_otpauth_uri(uri: &str) -> AppResult<ParsedImport> {
    if uri.len() > 16_384 {
        return Err(AppError::invalid("TOTP 链接过长。"));
    }
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| AppError::invalid("请输入有效的 otpauth://totp 链接。"))?;
    if scheme.eq_ignore_ascii_case("otpauth-migration") {
        return Err(AppError::new(
            "mfa_migration_unsupported",
            "第一版只支持标准单账户 TOTP 二维码，不支持 Google 批量迁移二维码。",
        ));
    }
    if !scheme.eq_ignore_ascii_case("otpauth") {
        return Err(AppError::invalid(
            "第一版只支持标准 otpauth://totp 单账户链接。",
        ));
    }
    let (without_fragment, _) = rest
        .split_once('#')
        .map_or((rest, None), |(head, _)| (head, Some(())));
    if rest.contains('#') {
        return Err(AppError::invalid("TOTP 链接格式无效。"));
    }
    let (authority_path, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    let (host, encoded_label) = authority_path
        .split_once('/')
        .ok_or_else(|| AppError::invalid("TOTP 链接格式无效。"))?;
    if !host.eq_ignore_ascii_case("totp") || encoded_label.is_empty() || encoded_label.contains('/')
    {
        return Err(AppError::invalid(
            "第一版只支持 otpauth://totp 单账户链接。",
        ));
    }
    let label =
        percent_decode(encoded_label).ok_or_else(|| AppError::invalid("TOTP 标签格式无效。"))?;
    let (label_issuer, account) = label
        .split_once(':')
        .map(|(issuer, account)| (issuer.trim().to_string(), account.trim().to_string()))
        .unwrap_or_else(|| (String::new(), label.trim().to_string()));
    if account.is_empty() || account.contains(':') {
        return Err(AppError::invalid("TOTP 账户标签不能为空。"));
    }

    let mut params: HashMap<String, String> = HashMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let key = key.to_ascii_lowercase();
        if matches!(
            key.as_str(),
            "secret" | "issuer" | "algorithm" | "digits" | "period"
        ) && params.insert(key.clone(), value.into_owned()).is_some()
        {
            return Err(AppError::invalid("TOTP 链接包含重复参数。"));
        }
    }
    let secret_text = Zeroizing::new(
        params
            .remove("secret")
            .ok_or_else(|| AppError::invalid("TOTP 链接缺少密钥。"))?,
    );
    let secret = decode_base32(&secret_text)?;
    let query_issuer = params
        .remove("issuer")
        .unwrap_or_default()
        .trim()
        .to_string();
    let issuer = if !query_issuer.is_empty() {
        query_issuer.clone()
    } else {
        label_issuer.clone()
    };
    let algorithm = params
        .remove("algorithm")
        .map(|value| {
            MfaAlgorithm::parse(&value).ok_or_else(|| AppError::invalid("TOTP 算法不受支持。"))
        })
        .transpose()?
        .unwrap_or_default();
    let digits = params
        .remove("digits")
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| AppError::invalid("TOTP 位数无效。"))
        })
        .transpose()?
        .unwrap_or(6);
    let period = params
        .remove("period")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| AppError::invalid("TOTP 周期无效。"))
        })
        .transpose()?
        .unwrap_or(30);
    let mut warnings = Vec::new();
    if !label_issuer.is_empty() && !query_issuer.is_empty() && label_issuer != query_issuer {
        warnings.push("链接中的发行方标记不一致，请确认账户归属。".to_string());
    }
    let name = if !issuer.is_empty() {
        issuer.clone()
    } else {
        account.clone()
    };
    let mut parsed = parse_entry(ParsedEntryInput {
        name,
        issuer,
        account,
        icon: "🔐".to_string(),
        algorithm,
        digits,
        period,
        secret,
    })?;
    parsed.warnings.extend(warnings);
    Ok(parsed)
}

fn parse_entry(input: ParsedEntryInput) -> AppResult<ParsedImport> {
    let ParsedEntryInput {
        name,
        issuer,
        account,
        icon,
        algorithm,
        digits,
        period,
        secret,
    } = input;
    if !matches!(digits, 6..=8) {
        return Err(AppError::invalid("TOTP 位数必须为 6、7 或 8。"));
    }
    if !(1..=3_600).contains(&period) {
        return Err(AppError::invalid("TOTP 周期必须在 1 到 3600 秒之间。"));
    }
    if secret.len() < 10 || secret.len() > 128 {
        return Err(AppError::invalid("TOTP 密钥长度无效。"));
    }
    let issuer = normalize_label_text(&issuer, 256, "发行方")?;
    let account = normalize_label_text(&account, 256, "账户")?;
    let mut name = normalize_label_text(&name, 120, "账户名称")?;
    if name.is_empty() {
        name = if !issuer.is_empty() {
            issuer.clone()
        } else if !account.is_empty() {
            account.clone()
        } else {
            "未命名账户".to_string()
        };
    }
    if account.is_empty() {
        return Err(AppError::invalid("TOTP 账户不能为空。"));
    }
    let mut warnings = Vec::new();
    if secret.len() < 20 {
        warnings.push("密钥长度低于 RFC 推荐值，请确认服务方设置。".to_string());
    }
    Ok(ParsedImport {
        entry: StoredEntry {
            id: Uuid::new_v4().to_string(),
            name,
            issuer,
            account_name: account,
            icon_emoji: normalize_icon(&icon),
            algorithm,
            digits,
            period,
            created_at: now_iso(),
            updated_at: now_iso(),
            secret,
        },
        warnings,
    })
}

fn decode_base32(value: &str) -> AppResult<Vec<u8>> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 512 || trimmed.chars().any(char::is_whitespace) {
        return Err(AppError::invalid("TOTP 密钥格式无效。"));
    }
    let normalized = Zeroizing::new(trimmed.trim_end_matches('=').to_ascii_uppercase());
    if normalized.is_empty()
        || normalized
            .chars()
            .any(|character| !character.is_ascii_alphanumeric())
    {
        return Err(AppError::invalid("TOTP 密钥格式无效。"));
    }
    BASE32_NOPAD
        .decode(normalized.as_bytes())
        .map_err(|_| AppError::invalid("TOTP 密钥格式无效。"))
}

fn normalize_label_text(value: &str, max_chars: usize, field: &str) -> AppResult<String> {
    let value = value.trim();
    if value.chars().any(|character| character.is_control()) {
        return Err(AppError::invalid(format!("{field}包含无效字符。")));
    }
    if value.chars().count() > max_chars {
        return Err(AppError::invalid(format!("{field}过长。")));
    }
    Ok(value.to_string())
}

fn normalize_icon(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 8 || value.chars().any(char::is_control) {
        "🔐".to_string()
    } else {
        value.to_string()
    }
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()?;
    let value = bytes.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_string())
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn purge_expired_imports(imports: &mut HashMap<String, PendingImport>) {
    let now = Instant::now();
    imports.retain(|_, pending| pending.expires_at > now);
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ensure_mfa_window(window: &WebviewWindow) -> AppResult<()> {
    if window.label() == "mfa" {
        Ok(())
    } else {
        Err(AppError::new(
            "mfa_window_required",
            "此操作只能在 MFA 验证器窗口中执行。",
        ))
    }
}

#[cfg(windows)]
fn protect_key(key: &[u8]) -> AppResult<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let input = CRYPT_INTEGER_BLOB {
        cbData: key.len() as u32,
        pbData: key.as_ptr() as *mut u8,
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() || output.cbData == 0 {
        return Err(generic_vault_error());
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(output.pbData.cast::<c_void>());
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn protect_key(_key: &[u8]) -> AppResult<Vec<u8>> {
    Err(AppError::new(
        "unsupported_platform",
        "MFA 验证器仅支持 Windows 用户 DPAPI。",
    ))
}

#[cfg(windows)]
fn unprotect_key(wrapped: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: wrapped.len() as u32,
        pbData: wrapped.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() || output.cbData == 0 {
        return Err(generic_vault_error());
    }
    let output_bytes =
        unsafe { std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize) };
    let bytes = Zeroizing::new(output_bytes.to_vec());
    output_bytes.zeroize();
    unsafe {
        let _ = LocalFree(output.pbData.cast::<c_void>());
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn unprotect_key(_wrapped: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
    Err(AppError::new(
        "unsupported_platform",
        "MFA 验证器仅支持 Windows 用户 DPAPI。",
    ))
}

fn write_code_to_clipboard(
    code: &str,
    valid_until: u64,
    lease: &std::sync::Arc<Mutex<Option<ClipboardLease>>>,
) -> AppResult<()> {
    #[cfg(windows)]
    {
        write_code_to_clipboard_windows(code, valid_until, lease)
    }
    #[cfg(not(windows))]
    {
        let _ = (code, valid_until, lease);
        Err(AppError::new(
            "unsupported_platform",
            "MFA 剪贴板仅支持 Windows。",
        ))
    }
}

fn force_expire_clipboard(lease: &std::sync::Arc<Mutex<Option<ClipboardLease>>>) {
    if let Some(value) = lock_unpoisoned(lease).as_mut() {
        value.clear_at = Instant::now();
    }
}

fn schedule_clipboard_cleanup(lease: Arc<Mutex<Option<ClipboardLease>>>, clear_at: Instant) {
    std::thread::spawn(move || {
        std::thread::sleep(clear_at.saturating_duration_since(Instant::now()));
        let stop_at = Instant::now() + CLIPBOARD_CLEANUP_TIMEOUT;
        let mut retry_delay = Duration::from_millis(100);
        loop {
            #[cfg(windows)]
            let outcome = try_clear_owned_clipboard_windows(&lease);
            #[cfg(not(windows))]
            let outcome: AppResult<bool> = {
                lock_unpoisoned(&lease).take();
                Ok(true)
            };
            match outcome {
                Ok(true) => break,
                Ok(false) | Err(_) => {
                    // Clipboard owners occasionally hold it across message
                    // processing. Retry on a detached worker, but keep the
                    // lifetime bounded even if a broken clipboard owner never
                    // releases its handle.
                    if Instant::now() >= stop_at {
                        break;
                    }
                    std::thread::sleep(retry_delay);
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                }
            }
        }
    });
}

fn clear_clipboard_now(lease: &Arc<Mutex<Option<ClipboardLease>>>) -> bool {
    #[cfg(windows)]
    {
        let mut retry_delay = Duration::from_millis(20);
        for _ in 0..5 {
            match try_clear_owned_clipboard_windows(lease) {
                Ok(true) => return true,
                Ok(false) | Err(_) => std::thread::sleep(retry_delay),
            }
            retry_delay = (retry_delay * 2).min(Duration::from_millis(160));
        }
        return lock_unpoisoned(lease).is_none();
    }
    #[cfg(not(windows))]
    {
        lock_unpoisoned(lease).take();
        true
    }
}

#[cfg(windows)]
fn write_code_to_clipboard_windows(
    code: &str,
    valid_until: u64,
    lease: &std::sync::Arc<Mutex<Option<ClipboardLease>>>,
) -> AppResult<()> {
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
        RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    const CF_UNICODETEXT: u32 = 13;
    const MARKER_FORMAT: &str = "PetalDesk.MFA.ClipboardMarker.v1";
    const EXCLUDE_FORMAT: &str = "ExcludeClipboardContentFromMonitorProcessing";
    const HISTORY_FORMAT: &str = "CanIncludeInClipboardHistory";
    const CLOUD_FORMAT: &str = "CanUploadToCloudClipboard";

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    struct GlobalMemory(HGLOBAL);
    impl GlobalMemory {
        fn from_bytes(bytes: &[u8]) -> AppResult<Self> {
            let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
            if handle.is_null() {
                return Err(AppError::new("clipboard_error", "分配剪贴板内存失败。"));
            }
            let destination = unsafe { GlobalLock(handle) };
            if destination.is_null() {
                unsafe {
                    let _ = GlobalFree(handle);
                }
                return Err(AppError::new("clipboard_error", "锁定剪贴板内存失败。"));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    destination.cast::<u8>(),
                    bytes.len(),
                );
                let _ = GlobalUnlock(handle);
            }
            Ok(Self(handle))
        }
        fn transfer(mut self) -> HGLOBAL {
            let handle = self.0;
            self.0 = null_mut();
            handle
        }
    }
    impl Drop for GlobalMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = GlobalFree(self.0);
                }
            }
        }
    }
    fn register(name: &str) -> AppResult<u32> {
        let wide = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let value = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        if value == 0 {
            Err(AppError::new("clipboard_error", "注册剪贴板格式失败。"))
        } else {
            Ok(value)
        }
    }

    let mut utf16 = Zeroizing::new(code.encode_utf16().collect::<Vec<_>>());
    utf16.push(0);
    let text_bytes = unsafe {
        std::slice::from_raw_parts(utf16.as_ptr().cast::<u8>(), utf16.len() * size_of::<u16>())
    };
    let marker = Uuid::new_v4().to_string().into_bytes();
    let text_memory = GlobalMemory::from_bytes(text_bytes)?;
    let marker_memory = GlobalMemory::from_bytes(&marker)?;
    let zero = 0u32.to_ne_bytes();
    let exclude_memory = GlobalMemory::from_bytes(&zero)?;
    let history_memory = GlobalMemory::from_bytes(&zero)?;
    let cloud_memory = GlobalMemory::from_bytes(&zero)?;
    let marker_format = register(MARKER_FORMAT)?;
    let exclude_format = register(EXCLUDE_FORMAT)?;
    let history_format = register(HISTORY_FORMAT)?;
    let cloud_format = register(CLOUD_FORMAT)?;

    let mut opened = false;
    for _ in 0..CLIPBOARD_RETRY_COUNT {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(CLIPBOARD_RETRY_DELAY);
    }
    if !opened {
        return Err(AppError::new(
            "clipboard_busy",
            "剪贴板正被其他程序占用，请稍后重试。",
        ));
    }
    let _guard = ClipboardGuard;
    if unsafe { EmptyClipboard() } == 0 {
        return Err(AppError::new("clipboard_error", "清空剪贴板失败。"));
    }
    for (format, memory) in [
        (CF_UNICODETEXT, text_memory),
        (marker_format, marker_memory),
        (exclude_format, exclude_memory),
        (history_format, history_memory),
        (cloud_format, cloud_memory),
    ] {
        let handle = memory.transfer();
        if unsafe { SetClipboardData(format, handle) }.is_null() {
            unsafe {
                let _ = GlobalFree(handle);
                let _ = EmptyClipboard();
            }
            return Err(AppError::new("clipboard_error", "写入剪贴板失败。"));
        }
    }
    drop(_guard);
    let sequence = unsafe { GetClipboardSequenceNumber() };
    let clear_after = unix_seconds();
    let remaining = valid_until.saturating_sub(clear_after);
    let clear_at =
        Instant::now() + CLIPBOARD_MAX_SECONDS.min(Duration::from_secs(remaining.max(1)));
    *lock_unpoisoned(lease) = Some(ClipboardLease {
        sequence,
        marker: marker.clone(),
        clear_at,
    });
    // Replace the old lease and let its worker observe the newer deadline.
    // The worker owns only a marker and timestamp, never the OTP itself.
    schedule_clipboard_cleanup(lease.clone(), clear_at);
    Ok(())
}

#[cfg(windows)]
fn try_clear_owned_clipboard_windows(
    lease: &Arc<Mutex<Option<ClipboardLease>>>,
) -> AppResult<bool> {
    use std::ptr::null_mut;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
        IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    let expected = {
        let guard = lock_unpoisoned(lease);
        let Some(value) = guard.as_ref() else {
            return Ok(true);
        };
        if value.clear_at > Instant::now() {
            // This is an older cleanup worker observing a newer copy.
            return Ok(true);
        }
        (value.sequence, value.marker.clone())
    };
    if unsafe { GetClipboardSequenceNumber() } != expected.0 {
        clear_matching_lease(lease, expected.0, &expected.1);
        return Ok(true);
    }
    let name = "PetalDesk.MFA.ClipboardMarker.v1"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if format == 0 || unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return Ok(false);
    }
    let mut opened = false;
    for _ in 0..CLIPBOARD_RETRY_COUNT {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(CLIPBOARD_RETRY_DELAY);
    }
    if !opened {
        return Ok(false);
    }
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    let _guard = Guard;
    if unsafe { GetClipboardSequenceNumber() } != expected.0 {
        clear_matching_lease(lease, expected.0, &expected.1);
        return Ok(true);
    }
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return Ok(false);
    }
    let size = unsafe { GlobalSize(handle) };
    let ptr = unsafe { GlobalLock(handle) };
    let matches = !ptr.is_null()
        && size >= expected.1.len()
        && unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), expected.1.len()) == expected.1 };
    if !ptr.is_null() {
        unsafe {
            let _ = GlobalUnlock(handle);
        }
    }
    if !matches {
        clear_matching_lease(lease, expected.0, &expected.1);
        return Ok(true);
    }
    if unsafe { EmptyClipboard() } == 0 {
        return Ok(false);
    }
    clear_matching_lease(lease, expected.0, &expected.1);
    Ok(true)
}

fn clear_matching_lease(lease: &Arc<Mutex<Option<ClipboardLease>>>, sequence: u32, marker: &[u8]) {
    let mut guard = lock_unpoisoned(lease);
    if guard
        .as_ref()
        .is_some_and(|value| value.sequence == sequence && value.marker == marker)
    {
        guard.take();
    }
}

fn decode_qr_payloads(width: u32, height: u32, gray: &[u8]) -> AppResult<Vec<Vec<u8>>> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(generic_qr_error)?;
    if width == 0 || height == 0 || expected != gray.len() || expected > MAX_IMAGE_ALLOC as usize {
        return Err(generic_qr_error());
    }
    let mut decoder = Quirc::default();
    let codes = decoder.identify(width as usize, height as usize, gray);
    let mut payloads = Vec::new();
    for code in codes {
        let Ok(code) = code else {
            continue;
        };
        let Ok(decoded) = code.decode() else {
            continue;
        };
        if decoded.payload.len() <= 16_384 {
            payloads.push(decoded.payload);
        }
    }
    Ok(payloads)
}

#[cfg(windows)]
fn capture_mfa_monitor_luma() -> AppResult<(u32, u32, Vec<u8>)> {
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
        GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return Err(AppError::new(
            "mfa_capture_error",
            "读取鼠标所在显示器失败。",
        ));
    }
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return Err(AppError::new("mfa_capture_error", "定位显示器失败。"));
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return Err(AppError::new("mfa_capture_error", "读取显示器范围失败。"));
    }
    let width = u32::try_from(info.rcMonitor.right - info.rcMonitor.left)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(generic_qr_error)?;
    let height = u32::try_from(info.rcMonitor.bottom - info.rcMonitor.top)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(generic_qr_error)?;
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(generic_qr_error)?;
    if pixels > 40_000_000 {
        return Err(AppError::invalid("显示器画面过大，无法进行二维码识别。"));
    }
    let screen = unsafe { GetDC(null_mut()) };
    if screen.is_null() {
        return Err(AppError::new("mfa_capture_error", "获取桌面画面失败。"));
    }
    struct ScreenDc(*mut c_void);
    impl Drop for ScreenDc {
        fn drop(&mut self) {
            unsafe {
                let _ = ReleaseDC(null_mut(), self.0);
            }
        }
    }
    let screen = ScreenDc(screen);
    let memory = unsafe { CreateCompatibleDC(screen.0) };
    if memory.is_null() {
        return Err(AppError::new("mfa_capture_error", "创建屏幕缓冲区失败。"));
    }
    struct MemoryDc(*mut c_void);
    impl Drop for MemoryDc {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.0);
            }
        }
    }
    let memory = MemoryDc(memory);
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..BITMAPINFOHEADER::default()
        },
        ..BITMAPINFO::default()
    };
    let mut bits = null_mut();
    let bitmap = unsafe {
        CreateDIBSection(
            screen.0,
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            null_mut(),
            0,
        )
    };
    if bitmap.is_null() || bits.is_null() {
        return Err(AppError::new("mfa_capture_error", "创建像素缓冲区失败。"));
    }
    struct BitmapGuard {
        dc: *mut c_void,
        bitmap: *mut c_void,
        previous: *mut c_void,
    }
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.is_null() {
                    let _ = SelectObject(self.dc, self.previous);
                }
                let _ = DeleteObject(self.bitmap);
            }
        }
    }
    let selected = BitmapGuard {
        dc: memory.0,
        bitmap,
        previous: unsafe { SelectObject(memory.0, bitmap) },
    };
    if unsafe {
        BitBlt(
            memory.0,
            0,
            0,
            width as i32,
            height as i32,
            screen.0,
            info.rcMonitor.left,
            info.rcMonitor.top,
            SRCCOPY | CAPTUREBLT,
        )
    } == 0
    {
        return Err(AppError::new("mfa_capture_error", "复制显示器画面失败。"));
    }
    let bgra = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), pixels * 4) };
    let mut gray = Vec::with_capacity(pixels);
    for pixel in bgra.chunks_exact(4) {
        let y =
            (u32::from(pixel[2]) * 77 + u32::from(pixel[1]) * 150 + u32::from(pixel[0]) * 29 + 128)
                / 256;
        gray.push(y as u8);
    }
    bgra.zeroize();
    drop(selected);
    Ok((width, height, gray))
}

#[cfg(not(windows))]
fn capture_mfa_monitor_luma() -> AppResult<(u32, u32, Vec<u8>)> {
    Err(AppError::new(
        "unsupported_platform",
        "MFA 屏幕扫码仅支持 Windows。",
    ))
}

#[tauri::command]
pub async fn get_mfa_status(app: AppHandle, window: WebviewWindow) -> AppResult<MfaStatus> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || app.state::<MfaStore>().status_at(epoch))
        .await
        .map_err(|_| AppError::new("mfa_task_error", "读取 MFA 状态任务异常结束。"))?
}

#[tauri::command]
pub async fn list_mfa_entries(
    app: AppHandle,
    window: WebviewWindow,
) -> AppResult<Vec<MfaEntrySummary>> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || app.state::<MfaStore>().list_entries_at(epoch))
        .await
        .map_err(|_| AppError::new("mfa_task_error", "读取 MFA 账户任务异常结束。"))?
}

#[tauri::command]
pub fn preview_mfa_uri(
    store: State<'_, MfaStore>,
    window: WebviewWindow,
    uri: SensitiveText,
) -> AppResult<Vec<MfaImportPreview>> {
    ensure_mfa_window(&window)?;
    store.preview_uri(uri.as_str())
}

#[tauri::command]
pub fn preview_mfa_manual(
    store: State<'_, MfaStore>,
    window: WebviewWindow,
    request: MfaManualImportRequest,
) -> AppResult<Vec<MfaImportPreview>> {
    ensure_mfa_window(&window)?;
    store.preview_manual(request)
}

#[tauri::command]
pub async fn preview_mfa_qr_image(
    app: AppHandle,
    window: WebviewWindow,
    request: Request<'_>,
) -> AppResult<Vec<MfaImportPreview>> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    let mut bytes = match request.body() {
        InvokeBody::Raw(bytes) if bytes.len() <= MAX_IMAGE_BYTES => bytes.clone(),
        InvokeBody::Raw(_) => return Err(AppError::invalid("二维码图片过大，请选择较小的图片。")),
        InvokeBody::Json(_) => return Err(AppError::invalid("二维码图片必须以原始二进制提交。")),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let result = app.state::<MfaStore>().preview_image_at(&bytes, epoch);
        bytes.zeroize();
        result
    })
    .await
    .map_err(|_| generic_qr_error())?
}

#[tauri::command]
pub async fn scan_mfa_screen_qr(
    app: AppHandle,
    window: WebviewWindow,
) -> AppResult<Vec<MfaImportPreview>> {
    ensure_mfa_window(&window)?;
    let session_epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        let window = app.get_webview_window("mfa");
        let visible = window
            .as_ref()
            .and_then(|item| item.is_visible().ok())
            .unwrap_or(false);
        if visible {
            if let Some(item) = &window {
                let _ = item.hide();
            }
            #[cfg(windows)]
            unsafe {
                let _ = windows_sys::Win32::Graphics::Dwm::DwmFlush();
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        let result = match capture_mfa_monitor_luma() {
            Ok((width, height, mut gray)) => {
                let result = app.state::<MfaStore>().preview_luma_at_epoch(
                    width,
                    height,
                    &gray,
                    session_epoch,
                );
                gray.zeroize();
                result
            }
            Err(error) => Err(error),
        };
        if visible {
            if let Some(item) = &window {
                let _ = item.show();
                let _ = item.set_focus();
            }
        }
        result
    })
    .await
    .map_err(|_| AppError::new("mfa_capture_error", "屏幕扫码任务异常结束。"))?
}

#[tauri::command]
pub async fn commit_mfa_import(
    app: AppHandle,
    window: WebviewWindow,
    session_id: String,
    icon_emoji: String,
) -> AppResult<MfaEntrySummary> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>()
            .commit_import_at(&session_id, &icon_emoji, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "保存 MFA 账户任务异常结束。"))?
}

#[tauri::command]
pub fn cancel_mfa_import(
    store: State<'_, MfaStore>,
    window: WebviewWindow,
    session_id: String,
) -> AppResult<()> {
    ensure_mfa_window(&window)?;
    store.cancel_import(&session_id)
}

#[tauri::command]
pub async fn update_mfa_entry(
    app: AppHandle,
    window: WebviewWindow,
    request: MfaEntryUpdateRequest,
) -> AppResult<MfaEntrySummary> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().update_entry_at(request, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "更新 MFA 账户任务异常结束。"))?
}

#[tauri::command]
pub async fn delete_mfa_entry(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<()> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().delete_entry_at(&entry_id, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "删除 MFA 账户任务异常结束。"))?
}

#[tauri::command]
pub fn reveal_mfa_code(
    store: State<'_, MfaStore>,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<MfaRevealResult> {
    ensure_mfa_window(&window)?;
    store.reveal_code(&entry_id)
}

#[tauri::command]
pub async fn copy_mfa_code(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<()> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().copy_entry_code_at(&entry_id, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_copy_error", "复制验证码任务异常结束。"))?
}

#[tauri::command]
pub async fn lock_mfa_vault(app: AppHandle, window: WebviewWindow) -> AppResult<()> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().deactivate();
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().clear_deactivated_state(epoch);
        Ok(())
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "锁定 MFA 保险库任务异常结束。"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B public test vectors; these are not user credentials.
    const RFC_SECRET: &str = "12345678901234567890";

    #[test]
    fn rfc_sha1_vectors() {
        let secret = RFC_SECRET.as_bytes().to_vec();
        let totp = TOTP::new_unchecked(TotpAlgorithm::SHA1, 8, 0, 30, secret);
        let expected = [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ];
        for (timestamp, code) in expected {
            assert_eq!(totp.generate(timestamp), code);
        }
    }

    #[test]
    fn strict_uri_defaults_and_duplicate_parameters() {
        let parsed = parse_otpauth_uri(
            "otpauth://totp/GitHub%3Aalice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        )
        .unwrap();
        assert_eq!(parsed.entry.algorithm, MfaAlgorithm::Sha1);
        assert_eq!(parsed.entry.digits, 6);
        assert_eq!(parsed.entry.period, 30);
        assert!(parse_otpauth_uri(
            "otpauth://totp/a?secret=JBSWY3DPEHPK3PXP&secret=JBSWY3DPEHPK3PXP"
        )
        .is_err());
        assert!(parse_otpauth_uri("otpauth-migration://offline?data=x").is_err());
    }

    #[test]
    fn issuer_conflict_is_a_warning_without_revealing_secret() {
        let parsed = parse_otpauth_uri("otpauth://totp/LabelIssuer%3Aalice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=QueryIssuer").unwrap();
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings.iter().any(|item| item.contains("发行方")));
    }

    #[test]
    fn rfc_sha256_and_sha512_vectors() {
        let sha256 = TOTP::new_unchecked(
            TotpAlgorithm::SHA256,
            8,
            0,
            30,
            b"12345678901234567890123456789012".to_vec(),
        );
        let sha512 = TOTP::new_unchecked(
            TotpAlgorithm::SHA512,
            8,
            0,
            30,
            b"1234567890123456789012345678901234567890123456789012345678901234".to_vec(),
        );
        for (timestamp, a, b) in [
            (59, "46119246", "90693936"),
            (1_111_111_109, "68084774", "25091201"),
            (1_111_111_111, "67062674", "99943326"),
            (1_234_567_890, "91819424", "93441116"),
            (2_000_000_000, "90698825", "38618901"),
            (20_000_000_000, "77737706", "47863826"),
        ] {
            assert_eq!(sha256.generate(timestamp), a);
            assert_eq!(sha512.generate(timestamp), b);
        }
    }

    #[test]
    fn invalid_secret_and_period_are_rejected() {
        assert!(decode_base32("not base32").is_err());
        assert!(parse_otpauth_uri("otpauth://totp/a?secret=JBSWY3DPEHPK3PXP&period=0").is_err());
        assert!(parse_otpauth_uri("otpauth://totp/a?secret=JBSWY3DPEHPK3PXP&digits=9").is_err());
    }

    #[test]
    fn qr_decoder_recognises_a_standard_single_account_uri() {
        use image::Luma;
        use qrcode::QrCode;
        let uri = "otpauth://totp/PetalDesk%3Arfc-test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk";
        let image = QrCode::new(uri.as_bytes())
            .unwrap()
            .render::<Luma<u8>>()
            .min_dimensions(320, 320)
            .build();
        let payloads = decode_qr_payloads(image.width(), image.height(), image.as_raw()).unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0], uri.as_bytes());
    }

    #[test]
    fn errors_never_echo_secret_or_uri() {
        let sensitive = "THIS-IS-NOT-BASE32-AND-MUST-NOT-ECHO";
        let uri = format!("otpauth://totp/test?secret={sensitive}&period=0");
        let error = match parse_otpauth_uri(&uri) {
            Ok(_) => panic!("invalid URI unexpectedly accepted"),
            Err(error) => error,
        };
        let displayed = error.to_string();
        assert!(!displayed.contains(sensitive));
        assert!(!displayed.contains("otpauth://"));
    }

    #[test]
    fn oversized_files_are_rejected_before_deserialization() {
        let root = tempfile::tempdir().unwrap();
        let vault_path = root.path().join("oversized-vault.json");
        let vault = std::fs::File::create(&vault_path).unwrap();
        vault.set_len(MAX_VAULT_BYTES as u64 + 1).unwrap();
        drop(vault);
        assert!(read_envelope(&vault_path).is_err());

        let store = MfaStore::load(root.path()).unwrap();
        let settings_path = store.vault_path.parent().unwrap().join(SETTINGS_FILE);
        drop(store);
        let settings = std::fs::File::create(&settings_path).unwrap();
        settings.set_len(MAX_SETTINGS_BYTES + 1).unwrap();
        drop(settings);

        let reloaded = MfaStore::load(root.path()).unwrap();
        let settings: MfaSettings =
            serde_json::from_slice(&std::fs::read(settings_path).unwrap()).unwrap();
        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
        drop(reloaded);
    }

    #[cfg(windows)]
    fn test_store() -> (tempfile::TempDir, MfaStore) {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        store.activate();
        (root, store)
    }

    #[cfg(windows)]
    fn import_public_rfc_entry(store: &MfaStore, account: &str) -> MfaEntrySummary {
        // RFC 6238 Appendix B's public test secret, never a real MFA account.
        let uri = format!(
            "otpauth://totp/PetalDesk%3A{account}?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk"
        );
        let preview = store.preview_uri(&uri).unwrap().remove(0);
        store.commit_import(&preview.session_id, "🌸").unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip_is_bound_to_the_current_windows_user() {
        let mut key = (0u8..32).collect::<Vec<_>>();
        let wrapped = protect_key(&key).unwrap();
        assert_ne!(wrapped, key);
        let mut recovered = unprotect_key(&wrapped).unwrap();
        assert_eq!(recovered.as_slice(), key.as_slice());
        key.zeroize();
        recovered.zeroize();
    }

    #[cfg(windows)]
    #[test]
    fn vault_is_encrypted_and_can_be_reopened() {
        let (_root, store) = test_store();
        let marker = "vault-plaintext-marker";
        let added = import_public_rfc_entry(&store, marker);
        let bytes = std::fs::read(&store.vault_path).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains(marker));
        assert!(!text.contains("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"));
        assert!(!text.contains(RFC_SECRET));
        store.lock();
        store.activate();
        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, added.id);
        assert_eq!(entries[0].account_name, marker);
    }

    #[cfg(windows)]
    #[test]
    fn tampered_aead_is_rejected_without_overwriting_the_vault() {
        let (root, store) = test_store();
        import_public_rfc_entry(&store, "tamper-test");
        for file in std::fs::read_dir(&store.backup_path).unwrap().flatten() {
            let _ = std::fs::remove_file(file.path());
        }
        let bytes = std::fs::read(&store.vault_path).unwrap();
        let mut envelope: VaultEnvelope = serde_json::from_slice(&bytes).unwrap();
        let mut ciphertext = STANDARD_NO_PAD
            .decode(envelope.ciphertext.as_bytes())
            .unwrap();
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x80;
        envelope.ciphertext = STANDARD_NO_PAD.encode(ciphertext);
        let tampered = serde_json::to_vec_pretty(&envelope).unwrap();
        atomic_write(&store.vault_path, &tampered).unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        let status = reopened.status().unwrap();
        assert!(!status.available);
        assert!(reopened.list_entries().is_err());
        assert_eq!(std::fs::read(&reopened.vault_path).unwrap(), tampered);
    }

    #[cfg(windows)]
    #[test]
    fn backup_rotation_keeps_five_and_recovers_an_authenticated_copy() {
        let (root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "backup-test");
        for index in 0..8 {
            store
                .update_entry(MfaEntryUpdateRequest {
                    id: entry.id.clone(),
                    name: format!("backup-name-{index}"),
                    issuer: "PetalDesk".to_string(),
                    account_name: "backup-test".to_string(),
                    icon_emoji: "🌸".to_string(),
                })
                .unwrap();
        }
        let backup_count = std::fs::read_dir(&store.backup_path)
            .unwrap()
            .filter_map(Result::ok)
            .count();
        assert_eq!(backup_count, 5);
        atomic_write(&store.vault_path, b"{\"schemaVersion\":1}").unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        let entries = reopened.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name.starts_with("backup-name-"));
        assert!(decrypt_envelope(&std::fs::read(&reopened.vault_path).unwrap()).is_ok());
        let preserved = std::fs::read_dir(reopened.vault_path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("vault.json.corrupt-")
            });
        assert!(preserved);
    }

    #[cfg(windows)]
    #[test]
    fn update_and_delete_roll_back_when_persistence_fails() {
        let (_root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "rollback-test");
        std::fs::remove_file(&store.vault_path).unwrap();
        std::fs::create_dir(&store.vault_path).unwrap();

        assert!(store
            .update_entry(MfaEntryUpdateRequest {
                id: entry.id.clone(),
                name: "must-not-stick".to_string(),
                issuer: "Changed".to_string(),
                account_name: "changed".to_string(),
                icon_emoji: "❌".to_string(),
            })
            .is_err());
        let after_update = store.list_entries().unwrap();
        assert_eq!(after_update.len(), 1);
        assert_eq!(after_update[0].name, "PetalDesk");
        assert_eq!(after_update[0].account_name, "rollback-test");

        assert!(store.delete_entry(&entry.id).is_err());
        let after_delete = store.list_entries().unwrap();
        assert_eq!(after_delete.len(), 1);
        assert_eq!(after_delete[0].id, entry.id);
    }

    #[cfg(windows)]
    #[test]
    fn import_sessions_are_single_use() {
        let (_root, store) = test_store();
        let preview = store
            .preview_uri("otpauth://totp/PetalDesk%3Aonce?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk")
            .unwrap()
            .remove(0);
        store.commit_import(&preview.session_id, "🔐").unwrap();
        assert!(store.commit_import(&preview.session_id, "🔐").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn entry_limit_rejects_data_that_the_next_launch_could_not_open() {
        let (_root, store) = test_store();
        {
            let mut runtime = lock_unpoisoned(&store.runtime);
            store.ensure_unlocked(&mut runtime).unwrap();
            let vault = runtime.vault.as_mut().unwrap();
            vault.payload.entries = (0..MAX_VAULT_ENTRIES)
                .map(|index| StoredEntry {
                    id: format!("entry-{index}"),
                    name: "test".to_string(),
                    issuer: String::new(),
                    account_name: String::new(),
                    icon_emoji: "🔐".to_string(),
                    algorithm: MfaAlgorithm::Sha1,
                    digits: 6,
                    period: 30,
                    created_at: "2026-01-01T00:00:00.000Z".to_string(),
                    updated_at: "2026-01-01T00:00:00.000Z".to_string(),
                    secret: RFC_SECRET.as_bytes().to_vec(),
                })
                .collect();
        }
        let preview = store
            .preview_uri("otpauth://totp/PetalDesk%3Alimit?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk")
            .unwrap()
            .remove(0);
        let error = store.commit_import(&preview.session_id, "🔐").unwrap_err();
        assert_eq!(error.code, "mfa_vault_entry_limit");
        assert_eq!(store.list_entries().unwrap().len(), MAX_VAULT_ENTRIES);
    }

    #[cfg(windows)]
    #[test]
    fn stale_epoch_cannot_reunlock_after_window_close_or_reopen() {
        let (_root, store) = test_store();
        import_public_rfc_entry(&store, "epoch-test");
        let old_epoch = store.require_active_epoch().unwrap();
        assert_eq!(store.activate(), old_epoch);
        let closing_epoch = store.deactivate();
        assert!(store.list_entries_at(old_epoch).is_err());
        store.clear_deactivated_state(closing_epoch);
        assert!(lock_unpoisoned(&store.runtime).vault.is_none());

        let new_epoch = store.activate();
        assert_ne!(new_epoch, closing_epoch);
        assert_eq!(store.list_entries_at(new_epoch).unwrap().len(), 1);
        // A delayed cleanup task from the destroyed old window must not wipe
        // a freshly reopened MFA session.
        store.clear_deactivated_state(closing_epoch);
        assert_eq!(store.list_entries_at(new_epoch).unwrap().len(), 1);
    }
}
