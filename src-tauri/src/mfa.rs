//! Local MFA (TOTP) vault with passwordless OS-backed access and portable recovery.
//!
//! This module deliberately keeps the account secret out of all public
//! serialised structures.  Only a short-lived reveal operation returns a
//! generated code to the webview.  The on-disk vault contains an
//! XChaCha20-Poly1305 envelope whose random data key is wrapped both by the
//! Windows DPAPI or macOS Keychain and by an Argon2id-derived recovery key.

use crate::error::{AppError, AppResult};
use crate::storage::{
    atomic_write, atomic_write_json, ensure_managed_subdirectory, INTERNAL_DATA_DIR,
};
use argon2::{Algorithm as Argon2Algorithm, Argon2, Params as Argon2Params, Version};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{SecondsFormat, Utc};
use data_encoding::BASE32_NOPAD;
use image::{ImageEncoder, ImageReader, Limits};
use quircs::Quirc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
#[cfg(windows)]
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
const CONFLICT_DIR: &str = "conflicts";
// Version 2 is still accepted so an existing encrypted vault can be opened;
// the next successful write upgrades its payload and envelope to version 3.
const VAULT_SCHEMA_VERSION: u32 = 3;
const LEGACY_VAULT_SCHEMA_VERSION: u32 = 2;
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const VAULT_AAD: &[u8] = b"PetalDesk MFA vault v2";
const RECOVERY_KEY_AAD: &[u8] = b"PetalDesk MFA recovery key v2";
const RECOVERY_KDF: &str = "argon2id";
const RECOVERY_KDF_VERSION: u32 = 19;
#[cfg(not(test))]
const RECOVERY_KDF_MEMORY_KIB: u32 = 64 * 1024;
#[cfg(test)]
const RECOVERY_KDF_MEMORY_KIB: u32 = 8 * 1024;
#[cfg(not(test))]
const RECOVERY_KDF_ITERATIONS: u32 = 3;
#[cfg(test)]
const RECOVERY_KDF_ITERATIONS: u32 = 1;
const RECOVERY_KDF_PARALLELISM: u32 = 1;
const RECOVERY_KDF_MIN_MEMORY_KIB: u32 = 8 * 1024;
const RECOVERY_KDF_MAX_MEMORY_KIB: u32 = 256 * 1024;
const RECOVERY_KDF_MAX_ITERATIONS: u32 = 10;
const RECOVERY_KDF_MAX_PARALLELISM: u32 = 4;
const RECOVERY_PASSWORD_MIN_CHARS: usize = 12;
const RECOVERY_PASSWORD_MAX_BYTES: usize = 1024;
const MAX_VAULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_VAULT_ENTRIES: usize = 10_000;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_WIDTH: u32 = 12_000;
const MAX_IMAGE_HEIGHT: u32 = 12_000;
const MAX_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;
const MAX_QR_SESSIONS: usize = 32;
// Batch URI import deliberately shares the QR/session ceiling.  This keeps a
// single paste from evicting all other pending imports or growing unbounded
// secret-bearing state in memory.
const MAX_BATCH_URI_BYTES: usize = 512 * 1024;
const IMPORT_SESSION_TTL: Duration = Duration::from_secs(5 * 60);
const CLIPBOARD_MAX_SECONDS: Duration = Duration::from_secs(30);
const CLIPBOARD_RETRY_COUNT: usize = 10;
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(20);
const CLIPBOARD_CLEANUP_TIMEOUT: Duration = Duration::from_secs(35);
#[cfg(any(target_os = "macos", test))]
const MACOS_CLIPBOARD_MARKER_SALT_BYTES: usize = 32;

fn generic_vault_error() -> AppError {
    AppError::new(
        "mfa_vault_unavailable",
        "MFA 数据保险库损坏或暂时无法读取；不会创建空白保险库。",
    )
}

fn recovery_password_required_error() -> AppError {
    AppError::new(
        "mfa_recovery_password_required",
        "此 MFA 保险库来自另一台电脑，请输入恢复密码完成迁移。",
    )
}

fn recovery_setup_required_error() -> AppError {
    AppError::new(
        "mfa_recovery_setup_required",
        "请先设置 MFA 恢复密码，再添加或修改账户。",
    )
}

fn invalid_recovery_password_error() -> AppError {
    AppError::new(
        "mfa_recovery_password_invalid",
        "恢复密码不正确，请重新输入。",
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
    pub recovery_state: MfaRecoveryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_excluded: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovered_from_backup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MfaRecoveryState {
    SetupRequired,
    Ready,
    PasswordRequired,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaEntrySummary {
    pub id: String,
    pub name: String,
    pub issuer: String,
    pub account_name: String,
    pub icon_emoji: String,
    pub pinned: bool,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaEntryExport {
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
    pub secret_base32: String,
    pub otpauth_uri: String,
    pub qr_png_data_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaTrashEntrySummary {
    pub id: String,
    pub name: String,
    pub issuer: String,
    pub account_name: String,
    pub icon_emoji: String,
    pub pinned: bool,
    pub algorithm: MfaAlgorithm,
    pub digits: u32,
    pub period: u64,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: String,
}

impl std::fmt::Debug for MfaEntryExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MfaEntryExport")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("issuer", &self.issuer)
            .field("account_name", &self.account_name)
            .field("icon_emoji", &self.icon_emoji)
            .field("algorithm", &self.algorithm)
            .field("digits", &self.digits)
            .field("period", &self.period)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("secret_base32", &"<redacted>")
            .field("otpauth_uri", &"<redacted>")
            .field("qr_png_data_url", &"<redacted>")
            .finish()
    }
}

impl Drop for MfaEntryExport {
    fn drop(&mut self) {
        self.secret_base32.zeroize();
        self.otpauth_uri.zeroize();
        self.qr_png_data_url.zeroize();
    }
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaImportLineError {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaBatchImportResult {
    pub previews: Vec<MfaImportPreview>,
    pub errors: Vec<MfaImportLineError>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaImportCommitRequest {
    pub session_id: String,
    pub icon_emoji: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultEnvelope {
    schema_version: u32,
    #[serde(default)]
    dpapi_wrapped_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keychain_key_id: Option<String>,
    recovery_wrapped_key: RecoveryKeyEnvelope,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryKeyEnvelope {
    kdf: String,
    kdf_version: u32,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultPayload {
    schema_version: u32,
    entries: Vec<StoredEntry>,
    #[serde(default)]
    trash: Vec<TrashedEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrashedEntry {
    deleted_at: String,
    entry: StoredEntry,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEntry {
    id: String,
    name: String,
    issuer: String,
    account_name: String,
    icon_emoji: String,
    #[serde(default)]
    pinned: bool,
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
    dpapi_wrapped_key: String,
    keychain_key_id: Option<String>,
    recovery_wrapped_key: Option<RecoveryKeyEnvelope>,
    disk_hash: Option<String>,
}

impl Drop for UnlockedVault {
    fn drop(&mut self) {
        self.dpapi_wrapped_key.zeroize();
        if let Some(keychain_key_id) = self.keychain_key_id.as_mut() {
            keychain_key_id.zeroize();
        }
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
    recovery_state: MfaRecoveryState,
    recovered_from_backup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalUnlockError {
    InvalidEnvelope,
    LocalKeyUnavailable,
    InvalidPayload,
}

enum LocalBackupResult {
    Found(Vec<u8>, UnlockedVault),
    PasswordRequired,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryUnlockError {
    InvalidEnvelope,
    InvalidPassword,
    InvalidPayload,
}

#[cfg(windows)]
fn local_protection_label(recovery_state: MfaRecoveryState) -> &'static str {
    match recovery_state {
        MfaRecoveryState::SetupRequired => "windows-dpapi",
        MfaRecoveryState::Ready | MfaRecoveryState::PasswordRequired => {
            "windows-dpapi-recovery-password"
        }
        MfaRecoveryState::Unavailable => "unavailable",
    }
}

#[cfg(target_os = "macos")]
fn local_protection_label(recovery_state: MfaRecoveryState) -> &'static str {
    match recovery_state {
        MfaRecoveryState::SetupRequired => "macos-keychain",
        MfaRecoveryState::Ready | MfaRecoveryState::PasswordRequired => {
            "macos-keychain-recovery-password"
        }
        MfaRecoveryState::Unavailable => "unavailable",
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn local_protection_label(_recovery_state: MfaRecoveryState) -> &'static str {
    "unavailable"
}

pub struct MfaStore {
    vault_path: PathBuf,
    backup_path: PathBuf,
    conflict_path: PathBuf,
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
        let root =
            ensure_managed_subdirectory(data_storage_path, &[INTERNAL_DATA_DIR, "tools", MFA_DIR])?;
        let backup_path = ensure_managed_subdirectory(
            data_storage_path,
            &[INTERNAL_DATA_DIR, "tools", MFA_DIR, BACKUP_DIR],
        )?;
        let conflict_path = ensure_managed_subdirectory(
            data_storage_path,
            &[INTERNAL_DATA_DIR, "tools", MFA_DIR, CONFLICT_DIR],
        )?;
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
        // Validate only the envelope shape here. Local key protection is
        // intentionally lazy so a copied vault can still open with recovery.
        let recovery_state = if vault_path.exists() {
            if read_envelope(&vault_path).is_ok() {
                MfaRecoveryState::Ready
            } else {
                MfaRecoveryState::Unavailable
            }
        } else {
            MfaRecoveryState::SetupRequired
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
            conflict_path,
            runtime: Mutex::new(RuntimeState {
                vault: None,
                recovery_state,
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
            protection: local_protection_label(runtime.recovery_state).to_string(),
            recovery_state: runtime.recovery_state,
            capture_excluded: match self.capture_excluded.load(Ordering::Acquire) {
                1 => Some(false),
                2 => Some(true),
                _ => None,
            },
            recovered_from_backup: runtime.recovered_from_backup,
            message: match runtime.recovery_state {
                MfaRecoveryState::SetupRequired => {
                    Some("请先设置恢复密码，之后即可添加 MFA 账户。".to_string())
                }
                MfaRecoveryState::Ready if runtime.recovered_from_backup => {
                    Some("MFA 主保险库缺失或损坏，已从最近的有效备份恢复。".to_string())
                }
                MfaRecoveryState::PasswordRequired => {
                    Some("此保险库缺少当前系统可用的本机密钥，请输入恢复密码完成迁移。".to_string())
                }
                MfaRecoveryState::Unavailable => {
                    Some("MFA 数据当前无法读取；不会创建空白保险库。".to_string())
                }
                MfaRecoveryState::Ready => None,
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

    #[cfg(test)]
    fn reorder_entries(&self, ordered_ids: Vec<String>) -> AppResult<Vec<MfaEntrySummary>> {
        let epoch = self.require_active_epoch()?;
        self.reorder_entries_at(ordered_ids, epoch)
    }

    fn reorder_entries_at(
        &self,
        ordered_ids: Vec<String>,
        epoch: u64,
    ) -> AppResult<Vec<MfaEntrySummary>> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        validate_complete_entry_order(&vault.payload.entries, &ordered_ids)?;

        let previous_order = vault
            .payload
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        apply_grouped_entry_order(&mut vault.payload.entries, &ordered_ids);
        let result = vault.payload.entries.iter().map(summary).collect();
        if vault
            .payload
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .eq(previous_order.iter().map(String::as_str))
        {
            return Ok(result);
        }
        if let Err(error) = self.save_vault(vault) {
            apply_exact_entry_order(&mut vault.payload.entries, &previous_order);
            return Err(error);
        }
        Ok(result)
    }

    #[cfg(test)]
    fn set_entry_pinned(&self, entry_id: &str, pinned: bool) -> AppResult<Vec<MfaEntrySummary>> {
        let epoch = self.require_active_epoch()?;
        self.set_entry_pinned_at(entry_id, pinned, epoch)
    }

    fn set_entry_pinned_at(
        &self,
        entry_id: &str,
        pinned: bool,
        epoch: u64,
    ) -> AppResult<Vec<MfaEntrySummary>> {
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
        if vault.payload.entries[index].pinned == pinned {
            return Ok(vault.payload.entries.iter().map(summary).collect());
        }

        let previous_order = vault
            .payload
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        let previous_pinned = vault.payload.entries[index].pinned;
        let mut entry = vault.payload.entries.remove(index);
        entry.pinned = pinned;
        let target_index = if pinned {
            0
        } else {
            vault
                .payload
                .entries
                .iter()
                .take_while(|entry| entry.pinned)
                .count()
        };
        vault.payload.entries.insert(target_index, entry);
        let result = vault.payload.entries.iter().map(summary).collect();
        if let Err(error) = self.save_vault(vault) {
            if let Some(entry) = vault
                .payload
                .entries
                .iter_mut()
                .find(|entry| entry.id == entry_id)
            {
                entry.pinned = previous_pinned;
            }
            apply_exact_entry_order(&mut vault.payload.entries, &previous_order);
            return Err(error);
        }
        Ok(result)
    }

    #[cfg(test)]
    fn configure_recovery_password(&self, password: &str) -> AppResult<MfaStatus> {
        let epoch = self.require_active_epoch()?;
        self.configure_recovery_password_at(password, None, epoch)
    }

    #[cfg(test)]
    fn change_recovery_password(
        &self,
        current_password: &str,
        password: &str,
    ) -> AppResult<MfaStatus> {
        let epoch = self.require_active_epoch()?;
        self.configure_recovery_password_at(password, Some(current_password), epoch)
    }

    fn configure_recovery_password_at(
        &self,
        password: &str,
        current_password: Option<&str>,
        epoch: u64,
    ) -> AppResult<MfaStatus> {
        validate_recovery_password(password)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        if vault.recovery_wrapped_key.is_some() {
            verify_current_recovery_password(
                vault,
                current_password.ok_or_else(invalid_recovery_password_error)?,
            )?;
        }
        let previous_wrapper = vault.recovery_wrapped_key.clone();
        vault.recovery_wrapped_key = Some(wrap_recovery_key(&vault.key, password)?);
        if let Err(error) = self.save_vault(vault) {
            vault.recovery_wrapped_key = previous_wrapper;
            return Err(error);
        }
        let current = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
        if vault
            .disk_hash
            .as_deref()
            .is_none_or(|expected| bytes_hash(&current) != expected)
        {
            return Err(AppError::new(
                "mfa_vault_conflict",
                "设置恢复密码后保险库又被外部修改，请重新打开并确认数据。",
            ));
        }
        self.reset_backups_to_current(&current)?;
        if std::fs::read(&self.vault_path)
            .ok()
            .is_none_or(|bytes| bytes_hash(&bytes) != bytes_hash(&current))
        {
            return Err(AppError::new(
                "mfa_vault_conflict",
                "设置恢复密码时保险库被外部修改，请重新打开并确认数据。",
            ));
        }
        runtime.recovery_state = MfaRecoveryState::Ready;
        runtime.recovered_from_backup = false;
        drop(runtime);
        drop(_lifecycle);
        self.status_at(epoch)
    }

    #[cfg(test)]
    fn unlock_with_recovery_password(&self, password: &str) -> AppResult<MfaStatus> {
        let epoch = self.require_active_epoch()?;
        self.unlock_with_recovery_password_at(password, epoch)
    }

    fn unlock_with_recovery_password_at(&self, password: &str, epoch: u64) -> AppResult<MfaStatus> {
        validate_recovery_password(password)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);

        if let Some(vault) = runtime.vault.as_ref() {
            let wrapper = vault
                .recovery_wrapped_key
                .as_ref()
                .ok_or_else(recovery_setup_required_error)?;
            let recovered =
                unwrap_recovery_key(wrapper, password).map_err(|error| match error {
                    RecoveryUnlockError::InvalidPassword => invalid_recovery_password_error(),
                    RecoveryUnlockError::InvalidEnvelope | RecoveryUnlockError::InvalidPayload => {
                        generic_vault_error()
                    }
                })?;
            if recovered.as_slice() != vault.key.as_slice() {
                return Err(invalid_recovery_password_error());
            }
            runtime.recovery_state = MfaRecoveryState::Ready;
            drop(runtime);
            drop(_lifecycle);
            return self.status_at(epoch);
        }

        let primary = read_bounded_vault_bytes(&self.vault_path).ok();
        let mut recovered_from_backup = false;
        let mut preserve_primary = false;
        let mut vault = match primary.as_deref() {
            Some(bytes) => match decrypt_envelope_with_recovery(bytes, password) {
                Ok(vault) => vault,
                // A structurally valid primary rejected this password. Do not
                // fall back to an older backup that may use a former password.
                Err(RecoveryUnlockError::InvalidPassword) => {
                    return Err(invalid_recovery_password_error())
                }
                Err(RecoveryUnlockError::InvalidEnvelope) => {
                    preserve_primary = true;
                    recovered_from_backup = true;
                    self.find_valid_backup_with_recovery(password)?
                        .ok_or_else(generic_vault_error)?
                        .1
                }
                Err(RecoveryUnlockError::InvalidPayload) => {
                    preserve_primary = true;
                    recovered_from_backup = true;
                    self.find_valid_backup_with_recovery(password)?
                        .ok_or_else(generic_vault_error)?
                        .1
                }
            },
            None => {
                recovered_from_backup = true;
                self.find_valid_backup_with_recovery(password)?
                    .ok_or_else(generic_vault_error)?
                    .1
            }
        };

        rebind_current_platform_local_key(&mut vault)?;
        let rebound = serialize_vault(&vault)?;
        if let Some(expected) = primary.as_deref() {
            let current = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
            if bytes_hash(&current) != bytes_hash(expected) {
                return Err(AppError::new(
                    "mfa_vault_conflict",
                    "恢复过程中 MFA 保险库被外部修改，请重新打开后再试。",
                ));
            }
            if preserve_primary {
                preserve_corrupt_bytes(&self.vault_path, expected)?;
            } else {
                self.rotate_backup()?;
            }
            // Re-check after the backup/preservation work.  A copied data
            // directory may be touched by another process while recovery is
            // running; never publish a rebound vault over that replacement.
            let current = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
            if bytes_hash(&current) != bytes_hash(expected) {
                return Err(AppError::new(
                    "mfa_vault_conflict",
                    "恢复过程中 MFA 保险库被外部修改，请重新打开后再试。",
                ));
            }
        } else if self.vault_path.exists() {
            return Err(AppError::new(
                "mfa_vault_conflict",
                "恢复过程中出现了新的 MFA 保险库，请重新打开后再试。",
            ));
        }
        atomic_write(&self.vault_path, &rebound)?;
        self.reset_backups_to_current(&rebound)?;
        if std::fs::read(&self.vault_path)
            .ok()
            .is_none_or(|bytes| bytes_hash(&bytes) != bytes_hash(&rebound))
        {
            return Err(AppError::new(
                "mfa_vault_conflict",
                "恢复过程中 MFA 保险库被外部修改，请重新打开后再试。",
            ));
        }
        vault.disk_hash = Some(bytes_hash(&rebound));
        runtime.vault = Some(vault);
        runtime.recovery_state = MfaRecoveryState::Ready;
        runtime.recovered_from_backup = recovered_from_backup;
        drop(runtime);
        drop(_lifecycle);
        self.status_at(epoch)
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

    /// Parses one standard otpauth URI per non-empty line.  Invalid lines are
    /// reported individually so a single malformed account does not discard
    /// the valid accounts in the same paste.  Secrets are never included in
    /// the result or in line-level error messages.
    pub fn preview_uris(&self, text: &str) -> AppResult<MfaBatchImportResult> {
        let epoch = self.require_active_epoch()?;
        if text.len() > MAX_BATCH_URI_BYTES {
            return Err(AppError::invalid("批量验证器链接内容过长，请分批导入。"));
        }

        let mut pending = Vec::new();
        let mut errors = Vec::new();
        let mut seen_links = HashMap::<[u8; 32], usize>::new();
        let mut non_empty_lines = 0usize;

        for (line_index, raw_line) in text.lines().enumerate() {
            let line_number = line_index + 1;
            // A copied first line can contain a UTF-8 BOM.  It is not part of
            // the URI and should not turn an otherwise valid paste invalid.
            let line = raw_line.trim().trim_start_matches('\u{feff}').trim();
            if line.is_empty() {
                continue;
            }
            non_empty_lines += 1;
            if non_empty_lines > MAX_QR_SESSIONS {
                errors.push(MfaImportLineError {
                    line: line_number,
                    message: format!(
                        "一次最多导入 {MAX_QR_SESSIONS} 个账户，本行及后续内容已跳过。"
                    ),
                });
                break;
            }

            let mut parsed = match parse_otpauth_uri(line) {
                Ok(value) => value,
                Err(error) => {
                    errors.push(MfaImportLineError {
                        line: line_number,
                        message: error.message,
                    });
                    continue;
                }
            };

            // Skip only an exactly repeated normalized line.  Two distinct
            // keys for the same issuer/account can be legitimate during key
            // rotation, so account display fields must not be used for
            // de-duplication.  Retain only a digest, never another plaintext
            // copy of the secret-bearing URI.
            let link_hash: [u8; 32] = Sha256::digest(line.as_bytes()).into();
            if let Some(first_line) = seen_links.get(&link_hash) {
                errors.push(MfaImportLineError {
                    line: line_number,
                    message: format!("与第 {first_line} 行链接重复，已跳过。"),
                });
                continue;
            }
            seen_links.insert(link_hash, line_number);

            // Batch imports intentionally get varied icons.  Single URI and
            // manual imports retain their existing explicit/default icon.
            parsed.entry.icon_emoji = random_batch_icon();
            pending.push(parsed);
        }

        let previews = self.remember_imports(pending, epoch)?;
        Ok(MfaBatchImportResult { previews, errors })
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
                    "未识别到可导入的 TOTP 账户。",
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
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        if vault
            .payload
            .entries
            .len()
            .saturating_add(vault.payload.trash.len())
            >= MAX_VAULT_ENTRIES
        {
            return Err(AppError::new(
                "mfa_vault_entry_limit",
                "MFA 账户数量已达到安全上限。",
            ));
        }

        let mut imports = lock_unpoisoned(&self.imports);
        purge_expired_imports(&mut imports);
        let pending = imports
            .remove(session_id)
            .ok_or_else(|| AppError::not_found("导入预览已经过期，请重新识别。"))?;
        let PendingImport {
            mut entry,
            expires_at,
        } = pending;
        let original_icon = entry.icon_emoji.clone();
        let original_updated_at = entry.updated_at.clone();
        entry.icon_emoji = normalize_icon(icon_emoji);
        entry.updated_at = now_iso();
        let summary = summary(&entry);
        vault.payload.entries.push(entry);
        if let Err(error) = self.save_vault(vault) {
            let mut entry = vault
                .payload
                .entries
                .pop()
                .expect("the pending MFA entry was appended immediately before save");
            entry.icon_emoji = original_icon;
            entry.updated_at = original_updated_at;
            imports.insert(session_id.to_string(), PendingImport { entry, expires_at });
            return Err(error);
        }
        Ok(summary)
    }

    #[cfg(test)]
    pub fn commit_imports(
        &self,
        requests: Vec<MfaImportCommitRequest>,
    ) -> AppResult<Vec<MfaEntrySummary>> {
        let epoch = self.require_active_epoch()?;
        self.commit_imports_at(requests, epoch)
    }

    fn commit_imports_at(
        &self,
        requests: Vec<MfaImportCommitRequest>,
        epoch: u64,
    ) -> AppResult<Vec<MfaEntrySummary>> {
        if requests.is_empty() {
            return Err(AppError::invalid("请选择至少一个要导入的 MFA 账户。"));
        }
        if requests.len() > MAX_QR_SESSIONS {
            return Err(AppError::invalid(format!(
                "一次最多导入 {MAX_QR_SESSIONS} 个 MFA 账户。"
            )));
        }
        let mut requested_ids = HashSet::with_capacity(requests.len());
        for request in &requests {
            if request.session_id.is_empty() || !requested_ids.insert(request.session_id.as_str()) {
                return Err(AppError::invalid("批量导入包含空白或重复的预览标识。"));
            }
        }

        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;

        // Validate the vault and its capacity before consuming any pending
        // session, so setup/password/capacity errors leave the preview intact.
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        if vault
            .payload
            .entries
            .len()
            .checked_add(vault.payload.trash.len())
            .and_then(|count| count.checked_add(requests.len()))
            .is_none_or(|count| count > MAX_VAULT_ENTRIES)
        {
            return Err(AppError::new(
                "mfa_vault_entry_limit",
                "MFA 账户数量已达到安全上限。",
            ));
        }

        let mut imports = lock_unpoisoned(&self.imports);
        purge_expired_imports(&mut imports);
        if requests
            .iter()
            .any(|request| !imports.contains_key(&request.session_id))
        {
            return Err(AppError::not_found(
                "一个或多个导入预览已经过期，请重新识别。",
            ));
        }

        let mut removed = Vec::with_capacity(requests.len());
        for request in &requests {
            let pending = imports
                .remove(&request.session_id)
                .expect("all pending MFA import sessions were checked above");
            removed.push((request.session_id.clone(), pending));
        }

        let restore_metadata = removed
            .iter()
            .map(|(session_id, pending)| {
                (
                    session_id.clone(),
                    pending.expires_at,
                    pending.entry.icon_emoji.clone(),
                    pending.entry.updated_at.clone(),
                )
            })
            .collect::<Vec<_>>();

        for ((_, pending), request) in removed.iter_mut().zip(&requests) {
            pending.entry.icon_emoji = normalize_icon(&request.icon_emoji);
            pending.entry.updated_at = now_iso();
        }
        let summaries = removed
            .iter()
            .map(|(_, pending)| summary(&pending.entry))
            .collect::<Vec<_>>();
        let original_entry_count = vault.payload.entries.len();
        vault
            .payload
            .entries
            .extend(removed.into_iter().map(|(_, pending)| pending.entry));

        if let Err(error) = self.save_vault(vault) {
            let rolled_back = vault.payload.entries.split_off(original_entry_count);
            for ((session_id, expires_at, original_icon, original_updated_at), mut entry) in
                restore_metadata.into_iter().zip(rolled_back)
            {
                entry.icon_emoji = original_icon;
                entry.updated_at = original_updated_at;
                imports.insert(session_id, PendingImport { entry, expires_at });
            }
            return Err(error);
        }

        Ok(summaries)
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
        vault.payload.trash.insert(
            0,
            TrashedEntry {
                deleted_at: now_iso(),
                entry: removed,
            },
        );
        if let Err(error) = self.save_vault(vault) {
            let trashed = vault.payload.trash.remove(0);
            vault.payload.entries.insert(index, trashed.entry);
            return Err(error);
        }
        Ok(())
    }

    fn list_trash_at(&self, epoch: u64) -> AppResult<Vec<MfaTrashEntrySummary>> {
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        Ok(vault.payload.trash.iter().map(trash_summary).collect())
    }

    fn restore_entry_at(&self, entry_id: &str, epoch: u64) -> AppResult<MfaEntrySummary> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let index = vault
            .payload
            .trash
            .iter()
            .position(|entry| entry.entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("回收站中没有找到这个 MFA 账户。"))?;
        let removed = vault.payload.trash.remove(index);
        let deleted_at = removed.deleted_at.clone();
        let result = summary(&removed.entry);
        let target_index = if removed.entry.pinned {
            0
        } else {
            vault
                .payload
                .entries
                .iter()
                .take_while(|entry| entry.pinned)
                .count()
        };
        vault.payload.entries.insert(target_index, removed.entry);
        if let Err(error) = self.save_vault(vault) {
            let restored = vault.payload.entries.remove(target_index);
            vault.payload.trash.insert(
                index,
                TrashedEntry {
                    deleted_at,
                    entry: restored,
                },
            );
            return Err(error);
        }
        Ok(result)
    }

    fn permanently_delete_entry_at(&self, entry_id: &str, epoch: u64) -> AppResult<()> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let index = vault
            .payload
            .trash
            .iter()
            .position(|entry| entry.entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("回收站中没有找到这个 MFA 账户。"))?;
        let removed = vault.payload.trash.remove(index);
        if let Err(error) = self.save_vault(vault) {
            vault.payload.trash.insert(index, removed);
            return Err(error);
        }
        Ok(())
    }

    fn empty_trash_at(&self, epoch: u64) -> AppResult<()> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let removed = std::mem::take(&mut vault.payload.trash);
        if let Err(error) = self.save_vault(vault) {
            vault.payload.trash = removed;
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    fn list_trash(&self) -> AppResult<Vec<MfaTrashEntrySummary>> {
        let epoch = self.require_active_epoch()?;
        self.list_trash_at(epoch)
    }

    #[cfg(test)]
    fn restore_entry(&self, entry_id: &str) -> AppResult<MfaEntrySummary> {
        let epoch = self.require_active_epoch()?;
        self.restore_entry_at(entry_id, epoch)
    }

    #[cfg(test)]
    fn permanently_delete_entry(&self, entry_id: &str) -> AppResult<()> {
        let epoch = self.require_active_epoch()?;
        self.permanently_delete_entry_at(entry_id, epoch)
    }

    #[cfg(test)]
    fn empty_trash(&self) -> AppResult<()> {
        let epoch = self.require_active_epoch()?;
        self.empty_trash_at(epoch)
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

    #[cfg(test)]
    fn export_entry(&self, entry_id: &str, password: &str) -> AppResult<MfaEntryExport> {
        let epoch = self.require_active_epoch()?;
        self.export_entry_at(entry_id, password, epoch)
    }

    fn export_entry_at(
        &self,
        entry_id: &str,
        password: &str,
        epoch: u64,
    ) -> AppResult<MfaEntryExport> {
        // Export intentionally uses one generic invalid-password response for
        // both malformed and incorrect attempts.  It must not reveal password
        // policy details or whether a requested entry exists before auth.
        if validate_recovery_password(password).is_err() {
            return Err(invalid_recovery_password_error());
        }
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked_at(&mut runtime, epoch)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        verify_current_recovery_password(vault, password)?;
        let entry = vault
            .payload
            .entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个 MFA 账户。"))?;
        let entry_summary = summary(entry);
        let secret = Zeroizing::new(entry.secret.clone());
        drop(runtime);

        let result = build_mfa_entry_export(entry_summary, &secret)?;
        self.validate_epoch(epoch)?;
        Ok(result)
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
            match self.find_valid_backup_local() {
                LocalBackupResult::Found(bytes, mut vault) => {
                    atomic_write(&self.vault_path, &bytes)?;
                    vault.disk_hash = Some(bytes_hash(&bytes));
                    runtime.recovery_state = MfaRecoveryState::Ready;
                    runtime.recovered_from_backup = true;
                    runtime.vault = Some(vault);
                    return Ok(());
                }
                LocalBackupResult::PasswordRequired => {
                    runtime.recovery_state = MfaRecoveryState::PasswordRequired;
                    return Err(recovery_password_required_error());
                }
                LocalBackupResult::None if self.backup_candidates_exist() => {
                    runtime.recovery_state = MfaRecoveryState::Unavailable;
                    return Err(generic_vault_error());
                }
                LocalBackupResult::None => {}
            }
            runtime.recovery_state = MfaRecoveryState::SetupRequired;
            runtime.vault = Some(new_empty_vault()?);
            return Ok(());
        }
        let primary_len = std::fs::metadata(&self.vault_path)
            .map_err(|_| generic_vault_error())?
            .len();
        if primary_len > MAX_VAULT_BYTES as u64 {
            match self.find_valid_backup_local() {
                LocalBackupResult::Found(bytes, mut vault) => {
                    preserve_corrupt_path(&self.vault_path)?;
                    atomic_write(&self.vault_path, &bytes)?;
                    vault.disk_hash = Some(bytes_hash(&bytes));
                    runtime.recovery_state = MfaRecoveryState::Ready;
                    runtime.recovered_from_backup = true;
                    runtime.vault = Some(vault);
                    return Ok(());
                }
                LocalBackupResult::PasswordRequired => {
                    runtime.recovery_state = MfaRecoveryState::PasswordRequired;
                    return Err(recovery_password_required_error());
                }
                LocalBackupResult::None => {}
            }
            runtime.recovery_state = MfaRecoveryState::Unavailable;
            return Err(generic_vault_error());
        }
        let primary = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
        match decrypt_envelope_local(&primary) {
            Ok(mut vault) => {
                vault.disk_hash = Some(bytes_hash(&primary));
                runtime.recovery_state = MfaRecoveryState::Ready;
                runtime.vault = Some(vault);
                Ok(())
            }
            Err(LocalUnlockError::LocalKeyUnavailable) => {
                runtime.recovery_state = MfaRecoveryState::PasswordRequired;
                Err(recovery_password_required_error())
            }
            Err(LocalUnlockError::InvalidEnvelope | LocalUnlockError::InvalidPayload) => {
                // Preserve the damaged primary and try encrypted backups.  A
                // backup is accepted only after DPAPI + AEAD authentication.
                match self.find_valid_backup_local() {
                    LocalBackupResult::Found(bytes, mut vault) => {
                        // Copy the damaged bytes first; never rename the only
                        // primary away before a replacement is durable.
                        preserve_corrupt_bytes(&self.vault_path, &primary)?;
                        atomic_write(&self.vault_path, &bytes)?;
                        vault.disk_hash = Some(bytes_hash(&bytes));
                        runtime.recovery_state = MfaRecoveryState::Ready;
                        runtime.recovered_from_backup = true;
                        runtime.vault = Some(vault);
                        Ok(())
                    }
                    LocalBackupResult::PasswordRequired => {
                        runtime.recovery_state = MfaRecoveryState::PasswordRequired;
                        Err(recovery_password_required_error())
                    }
                    LocalBackupResult::None => {
                        runtime.recovery_state = MfaRecoveryState::Unavailable;
                        Err(generic_vault_error())
                    }
                }
            }
        }
    }

    fn find_valid_backup_local(&self) -> LocalBackupResult {
        let Ok(entries) = std::fs::read_dir(&self.backup_path) else {
            return LocalBackupResult::None;
        };
        let mut files = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        let mut password_required = false;
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
            match decrypt_envelope_local(&bytes) {
                Ok(vault) => return LocalBackupResult::Found(bytes, vault),
                Err(LocalUnlockError::LocalKeyUnavailable) => password_required = true,
                Err(LocalUnlockError::InvalidEnvelope | LocalUnlockError::InvalidPayload) => {}
            }
        }
        if password_required {
            LocalBackupResult::PasswordRequired
        } else {
            LocalBackupResult::None
        }
    }

    fn find_valid_backup_with_recovery(
        &self,
        password: &str,
    ) -> AppResult<Option<(Vec<u8>, UnlockedVault)>> {
        let mut files = std::fs::read_dir(&self.backup_path)
            .map_err(|_| generic_vault_error())?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        for file in files {
            let Ok(bytes) = read_bounded_vault_bytes(&file.path()) else {
                continue;
            };
            if let Ok(vault) = decrypt_envelope_with_recovery(&bytes, password) {
                return Ok(Some((bytes, vault)));
            }
        }
        Ok(None)
    }

    fn backup_candidates_exist(&self) -> bool {
        match std::fs::read_dir(&self.backup_path) {
            Ok(entries) => {
                for entry in entries {
                    let Ok(entry) = entry else {
                        return true;
                    };
                    if entry.file_type().is_ok_and(|file_type| file_type.is_file())
                        && entry.path().extension().is_some_and(|ext| ext == "json")
                    {
                        return true;
                    }
                }
                false
            }
            // An unreadable backup directory cannot prove that this is a new
            // vault. Refuse to create an empty primary until it is inspected.
            Err(_) => true,
        }
    }

    fn save_vault(&self, vault: &mut UnlockedVault) -> AppResult<()> {
        let bytes = serialize_vault(vault)?;
        let disk_matches = match vault.disk_hash.as_deref() {
            Some(expected) => std::fs::read(&self.vault_path)
                .ok()
                .is_some_and(|current| bytes_hash(&current) == expected),
            None => !self.vault_path.exists(),
        };
        if !disk_matches {
            let conflict = self.write_conflict(&bytes)?;
            return Err(AppError::new(
                "mfa_vault_conflict",
                "MFA 保险库已被外部修改；本次更改已加密保存到冲突目录，未覆盖现有数据。",
            )
            .with_details(serde_json::json!({
                "conflictPath": conflict.to_string_lossy(),
            })));
        }
        self.rotate_backup()?;
        let disk_still_matches = match vault.disk_hash.as_deref() {
            Some(expected) => std::fs::read(&self.vault_path)
                .ok()
                .is_some_and(|current| bytes_hash(&current) == expected),
            None => !self.vault_path.exists(),
        };
        if !disk_still_matches {
            let conflict = self.write_conflict(&bytes)?;
            return Err(AppError::new(
                "mfa_vault_conflict",
                "MFA 保险库已被外部修改；本次更改已加密保存到冲突目录，未覆盖现有数据。",
            )
            .with_details(serde_json::json!({
                "conflictPath": conflict.to_string_lossy(),
            })));
        }
        atomic_write(&self.vault_path, &bytes)?;
        vault.disk_hash = Some(bytes_hash(&bytes));
        Ok(())
    }

    fn reset_backups_to_current(&self, bytes: &[u8]) -> AppResult<()> {
        let current = self.backup_path.join(format!(
            "vault-{}-{}.json",
            Utc::now().format("%Y%m%d%H%M%S%3f"),
            Uuid::new_v4()
        ));
        atomic_write(&current, bytes)?;
        for entry in std::fs::read_dir(&self.backup_path)
            .map_err(|error| AppError::io("读取 MFA 备份目录", error))?
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path != current && path.extension().is_some_and(|ext| ext == "json") {
                if std::fs::remove_file(&path).is_err() {
                    // A scanner may briefly hold the old backup open. Replace
                    // it with the newly wrapped snapshot so no retained JSON
                    // file remains bound to a former recovery password.
                    atomic_write(&path, bytes)?;
                }
            }
        }
        let expected_hash = bytes_hash(bytes);
        for entry in std::fs::read_dir(&self.backup_path)
            .map_err(|error| AppError::io("校验 MFA 备份目录", error))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        {
            let stored = read_bounded_vault_bytes(&entry.path())?;
            if bytes_hash(&stored) != expected_hash {
                return Err(AppError::new(
                    "mfa_vault_conflict",
                    "MFA 备份目录在更新恢复密码时被外部修改，请重新检查。",
                ));
            }
        }
        Ok(())
    }

    fn write_conflict(&self, bytes: &[u8]) -> AppResult<PathBuf> {
        let conflict = self.conflict_path.join(format!(
            "vault-{}-{}.json",
            Utc::now().format("%Y%m%d%H%M%S%3f"),
            Uuid::new_v4()
        ));
        atomic_write(&conflict, bytes)?;
        let mut files = std::fs::read_dir(&self.conflict_path)
            .map_err(|error| AppError::io("读取 MFA 冲突目录", error))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        for old in files.into_iter().skip(10) {
            let _ = std::fs::remove_file(old.path());
        }
        Ok(conflict)
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
    let mut vault = UnlockedVault {
        payload: VaultPayload {
            schema_version: VAULT_SCHEMA_VERSION,
            entries: Vec::new(),
            trash: Vec::new(),
        },
        key,
        dpapi_wrapped_key: String::new(),
        keychain_key_id: None,
        recovery_wrapped_key: None,
        disk_hash: None,
    };
    rebind_current_platform_local_key(&mut vault)?;
    Ok(vault)
}

fn read_envelope(path: &Path) -> AppResult<VaultEnvelope> {
    let bytes = read_bounded_vault_bytes(path)?;
    parse_envelope(&bytes).map_err(|_| generic_vault_error())
}

fn read_bounded_vault_bytes(path: &Path) -> AppResult<Vec<u8>> {
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
    Ok(bytes)
}

fn parse_envelope(bytes: &[u8]) -> Result<VaultEnvelope, LocalUnlockError> {
    if bytes.len() > MAX_VAULT_BYTES {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    let envelope: VaultEnvelope =
        serde_json::from_slice(bytes).map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    if !matches!(
        envelope.schema_version,
        LEGACY_VAULT_SCHEMA_VERSION | VAULT_SCHEMA_VERSION
    ) {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    validate_recovery_key_envelope(&envelope.recovery_wrapped_key)
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    if envelope
        .keychain_key_id
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 128)
    {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    Ok(envelope)
}

fn decrypt_envelope_local(bytes: &[u8]) -> Result<UnlockedVault, LocalUnlockError> {
    let envelope = parse_envelope(bytes)?;
    let nonce_bytes = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    if nonce_bytes.len() != 24 || ciphertext.is_empty() {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    let key = unprotect_local_key(
        &envelope.dpapi_wrapped_key,
        envelope.keychain_key_id.as_deref(),
    )
    .map_err(|_| LocalUnlockError::LocalKeyUnavailable)?;
    if key.len() != 32 {
        return Err(LocalUnlockError::LocalKeyUnavailable);
    }
    let payload = decrypt_vault_payload(&key, &nonce_bytes, &ciphertext)
        .map_err(|_| LocalUnlockError::InvalidPayload)?;
    Ok(UnlockedVault {
        payload,
        key,
        dpapi_wrapped_key: envelope.dpapi_wrapped_key,
        keychain_key_id: envelope.keychain_key_id,
        recovery_wrapped_key: Some(envelope.recovery_wrapped_key),
        disk_hash: Some(bytes_hash(bytes)),
    })
}

fn decrypt_envelope_with_recovery(
    bytes: &[u8],
    password: &str,
) -> Result<UnlockedVault, RecoveryUnlockError> {
    let envelope = parse_envelope(bytes).map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let key = unwrap_recovery_key(&envelope.recovery_wrapped_key, password)?;
    let nonce_bytes = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let payload = decrypt_vault_payload(&key, &nonce_bytes, &ciphertext)
        .map_err(|_| RecoveryUnlockError::InvalidPayload)?;
    Ok(UnlockedVault {
        payload,
        key,
        dpapi_wrapped_key: envelope.dpapi_wrapped_key,
        keychain_key_id: envelope.keychain_key_id,
        recovery_wrapped_key: Some(envelope.recovery_wrapped_key),
        disk_hash: Some(bytes_hash(bytes)),
    })
}

fn decrypt_vault_payload(
    key: &[u8],
    nonce_bytes: &[u8],
    ciphertext: &[u8],
) -> AppResult<VaultPayload> {
    if key.len() != 32 || nonce_bytes.len() != 24 || ciphertext.is_empty() {
        return Err(generic_vault_error());
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| generic_vault_error())?;
    let nonce_array: [u8; 24] = nonce_bytes.try_into().map_err(|_| generic_vault_error())?;
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
    let mut payload: VaultPayload =
        serde_json::from_slice(&plaintext).map_err(|_| generic_vault_error())?;
    if !matches!(
        payload.schema_version,
        LEGACY_VAULT_SCHEMA_VERSION | VAULT_SCHEMA_VERSION
    ) || payload.entries.len().saturating_add(payload.trash.len()) > MAX_VAULT_ENTRIES
    {
        return Err(generic_vault_error());
    }
    for entry in &payload.entries {
        validate_stored_entry(entry)?;
    }
    let mut ids = HashSet::with_capacity(payload.entries.len() + payload.trash.len());
    for entry in &payload.entries {
        if !ids.insert(entry.id.as_str()) {
            return Err(generic_vault_error());
        }
    }
    for deleted in &payload.trash {
        validate_stored_entry(&deleted.entry)?;
        if !is_valid_vault_timestamp(&deleted.deleted_at) || !ids.insert(deleted.entry.id.as_str())
        {
            return Err(generic_vault_error());
        }
    }
    payload.schema_version = VAULT_SCHEMA_VERSION;
    Ok(payload)
}

fn serialize_vault(vault: &UnlockedVault) -> AppResult<Vec<u8>> {
    let recovery_wrapped_key = vault
        .recovery_wrapped_key
        .clone()
        .ok_or_else(recovery_setup_required_error)?;
    if !has_current_platform_local_key(vault) {
        return Err(generic_vault_error());
    }
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
    let mut ciphertext = cipher
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
        dpapi_wrapped_key: vault.dpapi_wrapped_key.clone(),
        keychain_key_id: vault.keychain_key_id.clone(),
        recovery_wrapped_key,
        nonce: STANDARD_NO_PAD.encode(nonce_bytes),
        ciphertext: STANDARD_NO_PAD.encode(&ciphertext),
    };
    ciphertext.zeroize();
    let bytes = serde_json::to_vec_pretty(&envelope).map_err(|_| generic_vault_error())?;
    if bytes.len() > MAX_VAULT_BYTES {
        return Err(AppError::new(
            "mfa_vault_too_large",
            "MFA 数据保险库超过安全大小限制。",
        ));
    }
    Ok(bytes)
}

fn validate_recovery_password(password: &str) -> AppResult<()> {
    let chars = password.chars().count();
    if chars < RECOVERY_PASSWORD_MIN_CHARS || password.len() > RECOVERY_PASSWORD_MAX_BYTES {
        return Err(AppError::new(
            "mfa_recovery_password_policy",
            format!(
                "恢复密码至少需要 {RECOVERY_PASSWORD_MIN_CHARS} 个字符，且不能超过 {RECOVERY_PASSWORD_MAX_BYTES} 字节。"
            ),
        ));
    }
    Ok(())
}

fn validate_recovery_key_envelope(wrapper: &RecoveryKeyEnvelope) -> AppResult<()> {
    if wrapper.kdf != RECOVERY_KDF
        || wrapper.kdf_version != RECOVERY_KDF_VERSION
        || wrapper.memory_kib < RECOVERY_KDF_MIN_MEMORY_KIB
        || wrapper.memory_kib > RECOVERY_KDF_MAX_MEMORY_KIB
        || wrapper.iterations == 0
        || wrapper.iterations > RECOVERY_KDF_MAX_ITERATIONS
        || wrapper.parallelism == 0
        || wrapper.parallelism > RECOVERY_KDF_MAX_PARALLELISM
    {
        return Err(generic_vault_error());
    }
    let salt = STANDARD_NO_PAD
        .decode(wrapper.salt.as_bytes())
        .map_err(|_| generic_vault_error())?;
    let nonce = STANDARD_NO_PAD
        .decode(wrapper.nonce.as_bytes())
        .map_err(|_| generic_vault_error())?;
    let ciphertext = STANDARD_NO_PAD
        .decode(wrapper.ciphertext.as_bytes())
        .map_err(|_| generic_vault_error())?;
    if !(16..=64).contains(&salt.len()) || nonce.len() != 24 || ciphertext.len() != 48 {
        return Err(generic_vault_error());
    }
    Ok(())
}

fn derive_recovery_wrapping_key(
    password: &str,
    salt: &[u8],
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
) -> AppResult<Zeroizing<Vec<u8>>> {
    let params = Argon2Params::new(memory_kib, iterations, parallelism, Some(32))
        .map_err(|_| generic_vault_error())?;
    let argon2 = Argon2::new(Argon2Algorithm::Argon2id, Version::V0x13, params);
    let mut output = Zeroizing::new(vec![0u8; 32]);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut output)
        .map_err(|_| generic_vault_error())?;
    Ok(output)
}

fn wrap_recovery_key(key: &[u8], password: &str) -> AppResult<RecoveryKeyEnvelope> {
    validate_recovery_password(password)?;
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 24];
    getrandom::fill(&mut salt).map_err(|_| generic_vault_error())?;
    getrandom::fill(&mut nonce_bytes).map_err(|_| generic_vault_error())?;
    let wrapping_key = derive_recovery_wrapping_key(
        password,
        &salt,
        RECOVERY_KDF_MEMORY_KIB,
        RECOVERY_KDF_ITERATIONS,
        RECOVERY_KDF_PARALLELISM,
    )?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(&wrapping_key).map_err(|_| generic_vault_error())?;
    let nonce: XNonce = nonce_bytes.into();
    let mut ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: key,
                aad: RECOVERY_KEY_AAD,
            },
        )
        .map_err(|_| generic_vault_error())?;
    let wrapper = RecoveryKeyEnvelope {
        kdf: RECOVERY_KDF.to_string(),
        kdf_version: RECOVERY_KDF_VERSION,
        memory_kib: RECOVERY_KDF_MEMORY_KIB,
        iterations: RECOVERY_KDF_ITERATIONS,
        parallelism: RECOVERY_KDF_PARALLELISM,
        salt: STANDARD_NO_PAD.encode(salt),
        nonce: STANDARD_NO_PAD.encode(nonce_bytes),
        ciphertext: STANDARD_NO_PAD.encode(&ciphertext),
    };
    ciphertext.zeroize();
    Ok(wrapper)
}

fn unwrap_recovery_key(
    wrapper: &RecoveryKeyEnvelope,
    password: &str,
) -> Result<Zeroizing<Vec<u8>>, RecoveryUnlockError> {
    validate_recovery_key_envelope(wrapper).map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let salt = STANDARD_NO_PAD
        .decode(wrapper.salt.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let nonce_bytes = STANDARD_NO_PAD
        .decode(wrapper.nonce.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(wrapper.ciphertext.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let wrapping_key = derive_recovery_wrapping_key(
        password,
        &salt,
        wrapper.memory_kib,
        wrapper.iterations,
        wrapper.parallelism,
    )
    .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&wrapping_key)
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let nonce_array: [u8; 24] = nonce_bytes
        .as_slice()
        .try_into()
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let nonce: XNonce = nonce_array.into();
    let key = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: RECOVERY_KEY_AAD,
                },
            )
            .map_err(|_| RecoveryUnlockError::InvalidPassword)?,
    );
    if key.len() != 32 {
        return Err(RecoveryUnlockError::InvalidEnvelope);
    }
    Ok(key)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn verify_current_recovery_password(vault: &UnlockedVault, password: &str) -> AppResult<()> {
    let wrapper = vault
        .recovery_wrapped_key
        .as_ref()
        .ok_or_else(recovery_setup_required_error)?;
    let recovered = unwrap_recovery_key(wrapper, password).map_err(|error| match error {
        RecoveryUnlockError::InvalidPassword => invalid_recovery_password_error(),
        RecoveryUnlockError::InvalidEnvelope | RecoveryUnlockError::InvalidPayload => {
            generic_vault_error()
        }
    })?;
    if !constant_time_eq(&recovered, &vault.key) {
        return Err(invalid_recovery_password_error());
    }
    Ok(())
}

fn bytes_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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

fn is_valid_vault_timestamp(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TIMESTAMP_BYTES
        && chrono::DateTime::parse_from_rfc3339(value).is_ok()
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

fn mfa_export_error() -> AppError {
    AppError::new(
        "mfa_export_error",
        "无法生成这个账户的标准验证器导出，请检查账户信息。",
    )
}

fn algorithm_uri_name(algorithm: MfaAlgorithm) -> &'static str {
    match algorithm {
        MfaAlgorithm::Sha1 => "SHA1",
        MfaAlgorithm::Sha256 => "SHA256",
        MfaAlgorithm::Sha512 => "SHA512",
    }
}

fn build_otpauth_uri(summary: &MfaEntrySummary, secret_base32: &str) -> String {
    let account = if summary.account_name.is_empty() {
        summary.name.as_str()
    } else {
        summary.account_name.as_str()
    };
    let label = if summary.issuer.is_empty() {
        account.to_string()
    } else {
        format!("{}:{account}", summary.issuer)
    };
    let encoded_label =
        percent_encoding::utf8_percent_encode(&label, percent_encoding::NON_ALPHANUMERIC);
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("secret", secret_base32);
    query.append_pair("issuer", &summary.issuer);
    query.append_pair("algorithm", algorithm_uri_name(summary.algorithm));
    query.append_pair("digits", &summary.digits.to_string());
    query.append_pair("period", &summary.period.to_string());
    let query = Zeroizing::new(query.finish());
    format!("otpauth://totp/{encoded_label}?{}", query.as_str())
}

fn render_otpauth_qr_data_url(uri: &str) -> AppResult<String> {
    let code = qrcode::QrCode::with_error_correction_level(uri.as_bytes(), qrcode::EcLevel::M)
        .map_err(|_| mfa_export_error())?;
    let mut image = code
        .render::<image::Luma<u8>>()
        .min_dimensions(384, 384)
        .quiet_zone(true)
        .build();
    let mut png = Zeroizing::new(Vec::new());
    let encoded = image::codecs::png::PngEncoder::new(&mut *png).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::L8,
    );
    image.as_mut().fill(0);
    encoded.map_err(|_| mfa_export_error())?;
    let encoded_png = Zeroizing::new(STANDARD.encode(png.as_slice()));
    Ok(format!("data:image/png;base64,{}", encoded_png.as_str()))
}

fn build_mfa_entry_export(entry: MfaEntrySummary, secret: &[u8]) -> AppResult<MfaEntryExport> {
    let mut secret_base32 = Zeroizing::new(BASE32_NOPAD.encode(secret));
    let mut otpauth_uri = Zeroizing::new(build_otpauth_uri(&entry, &secret_base32));

    // Refuse to return a URI that our own strict importer cannot read back
    // exactly.  This catches unsupported labels before a misleading QR is
    // shown to the user.
    let parsed = parse_otpauth_uri(&otpauth_uri).map_err(|_| mfa_export_error())?;
    let expected_account = if entry.account_name.is_empty() {
        entry.name.as_str()
    } else {
        entry.account_name.as_str()
    };
    if parsed.entry.issuer != entry.issuer
        || parsed.entry.account_name != expected_account
        || parsed.entry.algorithm != entry.algorithm
        || parsed.entry.digits != entry.digits
        || parsed.entry.period != entry.period
        || !constant_time_eq(&parsed.entry.secret, secret)
    {
        return Err(mfa_export_error());
    }
    drop(parsed);

    let mut qr_png_data_url = Zeroizing::new(render_otpauth_qr_data_url(&otpauth_uri)?);
    Ok(MfaEntryExport {
        id: entry.id,
        name: entry.name,
        issuer: entry.issuer,
        account_name: entry.account_name,
        icon_emoji: entry.icon_emoji,
        algorithm: entry.algorithm,
        digits: entry.digits,
        period: entry.period,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        secret_base32: std::mem::take(&mut *secret_base32),
        otpauth_uri: std::mem::take(&mut *otpauth_uri),
        qr_png_data_url: std::mem::take(&mut *qr_png_data_url),
    })
}

fn summary(entry: &StoredEntry) -> MfaEntrySummary {
    MfaEntrySummary {
        id: entry.id.clone(),
        name: entry.name.clone(),
        issuer: entry.issuer.clone(),
        account_name: entry.account_name.clone(),
        icon_emoji: entry.icon_emoji.clone(),
        pinned: entry.pinned,
        algorithm: entry.algorithm,
        digits: entry.digits,
        period: entry.period,
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
    }
}

fn trash_summary(entry: &TrashedEntry) -> MfaTrashEntrySummary {
    let active = summary(&entry.entry);
    MfaTrashEntrySummary {
        id: active.id,
        name: active.name,
        issuer: active.issuer,
        account_name: active.account_name,
        icon_emoji: active.icon_emoji,
        pinned: active.pinned,
        algorithm: active.algorithm,
        digits: active.digits,
        period: active.period,
        created_at: active.created_at,
        updated_at: active.updated_at,
        deleted_at: entry.deleted_at.clone(),
    }
}

fn validate_complete_entry_order(entries: &[StoredEntry], ordered_ids: &[String]) -> AppResult<()> {
    if ordered_ids.len() != entries.len() {
        return Err(AppError::invalid("MFA 账户顺序与当前列表不匹配。"));
    }
    let current_ids = entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<HashSet<_>>();
    let mut requested_ids = HashSet::with_capacity(ordered_ids.len());
    for id in ordered_ids {
        if !current_ids.contains(id.as_str()) || !requested_ids.insert(id.as_str()) {
            return Err(AppError::invalid("MFA 账户顺序与当前列表不匹配。"));
        }
    }
    Ok(())
}

fn apply_exact_entry_order(entries: &mut [StoredEntry], ordered_ids: &[String]) {
    let positions = ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<HashMap<_, _>>();
    entries.sort_by_key(|entry| {
        positions
            .get(entry.id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
}

fn apply_grouped_entry_order(entries: &mut [StoredEntry], ordered_ids: &[String]) {
    let positions = ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect::<HashMap<_, _>>();
    entries.sort_by_key(|entry| {
        (
            !entry.pinned,
            positions
                .get(entry.id.as_str())
                .copied()
                .unwrap_or(usize::MAX),
        )
    });
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

const BATCH_IMPORT_ICONS: &[&str] = &[
    "🔐", "🔑", "🛡️", "🌸", "⭐", "💼", "🏠", "👤", "🐙", "☁️", "📧", "💬", "🛒", "🏦", "🎮", "🧰",
    "🟣", "🔵", "🟢", "🟡", "🟠", "🔴", "⚫", "⚪",
];

fn random_batch_icon() -> String {
    // Uuid v4 already uses a cryptographically random source in the platform
    // implementation, and avoids introducing another dependency just for a
    // cosmetic per-preview choice.
    let index = (Uuid::new_v4().as_u128() as usize) % BATCH_IMPORT_ICONS.len();
    BATCH_IMPORT_ICONS[index].to_string()
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
            "未识别到可导入的 TOTP 账户。",
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
            pinned: false,
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

#[cfg(any(windows, test))]
fn apply_dpapi_wrapper(vault: &mut UnlockedVault, wrapped: &[u8]) {
    vault.dpapi_wrapped_key = STANDARD_NO_PAD.encode(wrapped);
}

#[cfg(any(target_os = "macos", test))]
fn apply_keychain_key_id(vault: &mut UnlockedVault, key_id: String) {
    vault.keychain_key_id = Some(key_id);
}

#[cfg(windows)]
fn rebind_current_platform_local_key(vault: &mut UnlockedVault) -> AppResult<()> {
    let wrapped = protect_key(&vault.key)?;
    apply_dpapi_wrapper(vault, &wrapped);
    Ok(())
}

#[cfg(target_os = "macos")]
fn rebind_current_platform_local_key(vault: &mut UnlockedVault) -> AppResult<()> {
    let key_id = vault
        .keychain_key_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let entry =
        keyring::Entry::new("com.petaldesk.app.mfa", &key_id).map_err(|_| generic_vault_error())?;
    entry
        .set_secret(&vault.key)
        .map_err(|_| generic_vault_error())?;
    apply_keychain_key_id(vault, key_id);
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn rebind_current_platform_local_key(_vault: &mut UnlockedVault) -> AppResult<()> {
    Err(AppError::new(
        "unsupported_platform",
        "MFA 本机密钥保护仅支持 Windows 和 macOS。",
    ))
}

#[cfg(windows)]
fn has_current_platform_local_key(vault: &UnlockedVault) -> bool {
    !vault.dpapi_wrapped_key.is_empty()
}

#[cfg(target_os = "macos")]
fn has_current_platform_local_key(vault: &UnlockedVault) -> bool {
    vault
        .keychain_key_id
        .as_ref()
        .is_some_and(|value| !value.is_empty())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn has_current_platform_local_key(_vault: &UnlockedVault) -> bool {
    false
}

#[cfg(windows)]
fn unprotect_local_key(
    dpapi_wrapped_key: &str,
    _keychain_key_id: Option<&str>,
) -> AppResult<Zeroizing<Vec<u8>>> {
    let wrapped = STANDARD_NO_PAD
        .decode(dpapi_wrapped_key.as_bytes())
        .map_err(|_| generic_vault_error())?;
    if wrapped.is_empty() || wrapped.len() > 4096 {
        return Err(generic_vault_error());
    }
    unprotect_key(&wrapped)
}

#[cfg(target_os = "macos")]
fn unprotect_local_key(
    _dpapi_wrapped_key: &str,
    keychain_key_id: Option<&str>,
) -> AppResult<Zeroizing<Vec<u8>>> {
    let key_id = keychain_key_id.ok_or_else(generic_vault_error)?;
    let entry =
        keyring::Entry::new("com.petaldesk.app.mfa", key_id).map_err(|_| generic_vault_error())?;
    entry
        .get_secret()
        .map(Zeroizing::new)
        .map_err(|_| generic_vault_error())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unprotect_local_key(
    _dpapi_wrapped_key: &str,
    _keychain_key_id: Option<&str>,
) -> AppResult<Zeroizing<Vec<u8>>> {
    Err(AppError::new(
        "unsupported_platform",
        "MFA 本机密钥保护仅支持 Windows 和 macOS。",
    ))
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

#[cfg(not(any(windows, target_os = "macos")))]
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

#[cfg(not(any(windows, target_os = "macos")))]
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
    #[cfg(target_os = "macos")]
    {
        write_code_to_clipboard_macos(code, valid_until, lease)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
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
            #[cfg(target_os = "macos")]
            let outcome = try_clear_owned_clipboard_macos(&lease);
            #[cfg(not(any(windows, target_os = "macos")))]
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
    #[cfg(target_os = "macos")]
    {
        let mut retry_delay = Duration::from_millis(20);
        for _ in 0..5 {
            match try_clear_owned_clipboard_macos(lease) {
                Ok(true) => return true,
                Ok(false) | Err(_) => std::thread::sleep(retry_delay),
            }
            retry_delay = (retry_delay * 2).min(Duration::from_millis(160));
        }
        return lock_unpoisoned(lease).is_none();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        lock_unpoisoned(lease).take();
        true
    }
}

#[cfg(target_os = "macos")]
fn write_code_to_clipboard_macos(
    code: &str,
    valid_until: u64,
    lease: &Arc<Mutex<Option<ClipboardLease>>>,
) -> AppResult<()> {
    use arboard::SetExtApple;

    let marker = new_macos_clipboard_marker(code)?;
    let mut clipboard = arboard::Clipboard::new().map_err(|error| {
        AppError::new("clipboard_error", format!("打开 macOS 剪贴板失败: {error}"))
    })?;
    clipboard
        .set()
        .exclude_from_history()
        .text(code.to_string())
        .map_err(|error| {
            AppError::new("clipboard_error", format!("写入 macOS 剪贴板失败: {error}"))
        })?;
    let remaining = valid_until.saturating_sub(unix_seconds());
    let clear_at =
        Instant::now() + CLIPBOARD_MAX_SECONDS.min(Duration::from_secs(remaining.max(1)));
    *lock_unpoisoned(lease) = Some(ClipboardLease {
        sequence: 0,
        marker,
        clear_at,
    });
    schedule_clipboard_cleanup(lease.clone(), clear_at);
    Ok(())
}

#[cfg(target_os = "macos")]
fn try_clear_owned_clipboard_macos(lease: &Arc<Mutex<Option<ClipboardLease>>>) -> AppResult<bool> {
    let expected = {
        let guard = lock_unpoisoned(lease);
        let Some(value) = guard.as_ref() else {
            return Ok(true);
        };
        if value.clear_at > Instant::now() {
            return Ok(true);
        }
        value.marker.clone()
    };
    let mut clipboard = arboard::Clipboard::new().map_err(|error| {
        AppError::new("clipboard_error", format!("打开 macOS 剪贴板失败: {error}"))
    })?;
    let Ok(current) = clipboard.get_text() else {
        clear_matching_lease(lease, 0, &expected);
        return Ok(true);
    };
    if !macos_clipboard_marker_matches(&current, &expected) {
        clear_matching_lease(lease, 0, &expected);
        return Ok(true);
    }
    clipboard.clear().map_err(|error| {
        AppError::new("clipboard_error", format!("清理 macOS 剪贴板失败: {error}"))
    })?;
    clear_matching_lease(lease, 0, &expected);
    Ok(true)
}

#[cfg(target_os = "macos")]
fn new_macos_clipboard_marker(value: &str) -> AppResult<Vec<u8>> {
    let mut salt = [0_u8; MACOS_CLIPBOARD_MARKER_SALT_BYTES];
    getrandom::fill(&mut salt)
        .map_err(|_| AppError::new("clipboard_error", "生成 macOS 剪贴板所有权标记失败。"))?;
    Ok(macos_clipboard_marker_with_salt(value, &salt))
}

#[cfg(any(target_os = "macos", test))]
fn macos_clipboard_marker_with_salt(
    value: &str,
    salt: &[u8; MACOS_CLIPBOARD_MARKER_SALT_BYTES],
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();

    let mut marker = Vec::with_capacity(MACOS_CLIPBOARD_MARKER_SALT_BYTES + digest.len());
    marker.extend_from_slice(salt);
    marker.extend_from_slice(&digest);
    marker
}

#[cfg(any(target_os = "macos", test))]
fn macos_clipboard_marker_matches(value: &str, marker: &[u8]) -> bool {
    let expected_len = MACOS_CLIPBOARD_MARKER_SALT_BYTES + 32;
    if marker.len() != expected_len {
        return false;
    }
    let (salt, expected_digest) = marker.split_at(MACOS_CLIPBOARD_MARKER_SALT_BYTES);
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(value.as_bytes());
    hasher.finalize().as_slice() == expected_digest
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
fn capture_mfa_monitor_luma(_app: &AppHandle) -> AppResult<(u32, u32, Vec<u8>)> {
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

#[cfg(target_os = "macos")]
fn capture_mfa_monitor_luma(app: &AppHandle) -> AppResult<(u32, u32, Vec<u8>)> {
    let (bounds, mut rgba) = crate::screenshot::capture_cursor_monitor_rgba(app)?;
    let pixels = (bounds.width as usize)
        .checked_mul(bounds.height as usize)
        .ok_or_else(generic_qr_error)?;
    if pixels > 40_000_000 || rgba.len() != pixels * 4 {
        rgba.zeroize();
        return Err(AppError::invalid(
            "显示器画面过大或像素数据无效，无法进行二维码识别。",
        ));
    }
    let mut gray = Vec::with_capacity(pixels);
    for pixel in rgba.chunks_exact(4) {
        let y =
            (u32::from(pixel[0]) * 77 + u32::from(pixel[1]) * 150 + u32::from(pixel[2]) * 29 + 128)
                / 256;
        gray.push(y as u8);
    }
    rgba.zeroize();
    Ok((bounds.width, bounds.height, gray))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn capture_mfa_monitor_luma(_app: &AppHandle) -> AppResult<(u32, u32, Vec<u8>)> {
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
pub async fn reorder_mfa_entries(
    app: AppHandle,
    window: WebviewWindow,
    ordered_ids: Vec<String>,
) -> AppResult<Vec<MfaEntrySummary>> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>()
            .reorder_entries_at(ordered_ids, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "调整 MFA 账户顺序任务异常结束。"))?
}

#[tauri::command]
pub async fn set_mfa_entry_pinned(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
    pinned: bool,
) -> AppResult<Vec<MfaEntrySummary>> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>()
            .set_entry_pinned_at(&entry_id, pinned, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "更新 MFA 账户置顶状态任务异常结束。"))?
}

#[tauri::command]
pub async fn configure_mfa_recovery_password(
    app: AppHandle,
    window: WebviewWindow,
    password: SensitiveText,
    current_password: Option<SensitiveText>,
) -> AppResult<MfaStatus> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().configure_recovery_password_at(
            password.as_str(),
            current_password.as_ref().map(SensitiveText::as_str),
            epoch,
        )
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "设置 MFA 恢复密码任务异常结束。"))?
}

#[tauri::command]
pub async fn unlock_mfa_with_recovery_password(
    app: AppHandle,
    window: WebviewWindow,
    password: SensitiveText,
) -> AppResult<MfaStatus> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>()
            .unlock_with_recovery_password_at(password.as_str(), epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "恢复 MFA 保险库任务异常结束。"))?
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
pub fn preview_mfa_uris(
    store: State<'_, MfaStore>,
    window: WebviewWindow,
    uris: SensitiveText,
) -> AppResult<MfaBatchImportResult> {
    ensure_mfa_window(&window)?;
    store.preview_uris(uris.as_str())
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
        let result = match capture_mfa_monitor_luma(&app) {
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
pub async fn commit_mfa_imports(
    app: AppHandle,
    window: WebviewWindow,
    imports: Vec<MfaImportCommitRequest>,
) -> AppResult<Vec<MfaEntrySummary>> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().commit_imports_at(imports, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "批量保存 MFA 账户任务异常结束。"))?
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
pub async fn list_mfa_trash(
    app: AppHandle,
    window: WebviewWindow,
) -> AppResult<Vec<MfaTrashEntrySummary>> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || app.state::<MfaStore>().list_trash_at(epoch))
        .await
        .map_err(|_| AppError::new("mfa_task_error", "读取 MFA 回收站任务异常结束。"))?
}

#[tauri::command]
pub async fn restore_mfa_entry(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<MfaEntrySummary> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>().restore_entry_at(&entry_id, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "恢复 MFA 账户任务异常结束。"))?
}

#[tauri::command]
pub async fn permanently_delete_mfa_entry(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<()> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>()
            .permanently_delete_entry_at(&entry_id, epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "永久删除 MFA 账户任务异常结束。"))?
}

#[tauri::command]
pub async fn empty_mfa_trash(app: AppHandle, window: WebviewWindow) -> AppResult<()> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || app.state::<MfaStore>().empty_trash_at(epoch))
        .await
        .map_err(|_| AppError::new("mfa_task_error", "清空 MFA 回收站任务异常结束。"))?
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
pub async fn export_mfa_entry(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
    password: SensitiveText,
) -> AppResult<MfaEntryExport> {
    ensure_mfa_window(&window)?;
    let epoch = app.state::<MfaStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<MfaStore>()
            .export_entry_at(&entry_id, password.as_str(), epoch)
    })
    .await
    .map_err(|_| AppError::new("mfa_task_error", "导出 MFA 账户任务异常结束。"))?
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
    const RECOVERY_PASSWORD: &str = "petaldesk-test-recovery-password";

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
    fn macos_clipboard_marker_is_salted_and_does_not_store_the_code() {
        let code = "123456";
        let first_salt = [0x11; MACOS_CLIPBOARD_MARKER_SALT_BYTES];
        let second_salt = [0x22; MACOS_CLIPBOARD_MARKER_SALT_BYTES];
        let first = macos_clipboard_marker_with_salt(code, &first_salt);
        let second = macos_clipboard_marker_with_salt(code, &second_salt);

        assert_ne!(first, second);
        assert!(!first
            .windows(code.len())
            .any(|window| window == code.as_bytes()));
        assert_ne!(
            &first[MACOS_CLIPBOARD_MARKER_SALT_BYTES..],
            Sha256::digest(code.as_bytes()).as_slice()
        );
        assert!(macos_clipboard_marker_matches(code, &first));
        assert!(!macos_clipboard_marker_matches("654321", &first));
        assert!(!macos_clipboard_marker_matches(
            code,
            &first[..first.len() - 1]
        ));
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
    fn batch_uri_preview_reports_lines_and_only_deduplicates_identical_links() {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        store.activate();
        let first = "otpauth://totp/PetalDesk%3Aalice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk";
        // The same displayed account with a different key is legitimate, for
        // example while an MFA key is being rotated.
        let rotated = "otpauth://totp/PetalDesk%3Aalice?secret=JBSWY3DPEHPK3PXP&issuer=PetalDesk";
        let text = format!("\n{first}\r\nhttps://example.com/not-totp\n  {first}  \n{rotated}\n");

        let result = store.preview_uris(&text).unwrap();

        assert_eq!(result.previews.len(), 2);
        assert_eq!(result.errors.len(), 2);
        assert_eq!(result.errors[0].line, 3);
        assert_eq!(result.errors[1].line, 4);
        assert!(result.errors[1].message.contains("第 2 行"));
        assert_ne!(result.previews[0].session_id, result.previews[1].session_id);
        assert!(result.previews.iter().all(|preview| preview
            .icon_emoji
            .as_deref()
            .is_some_and(|icon| BATCH_IMPORT_ICONS.contains(&icon))));

        let serialized_errors = serde_json::to_string(&result.errors).unwrap();
        assert!(!serialized_errors.contains("GEZDGNBV"));
        assert!(!serialized_errors.contains("JBSWY3DP"));
    }

    #[test]
    fn batch_uri_preview_limits_secret_bearing_sessions() {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        store.activate();
        let mut text = (0..MAX_QR_SESSIONS)
            .map(|index| format!(
                "otpauth://totp/PetalDesk%3Aaccount-{index}?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk"
            ))
            .collect::<Vec<_>>()
            .join("\n");
        // Stay below the byte limit while exercising a line count large
        // enough to expose response amplification if every overflow line were
        // returned to the WebView separately.
        text.push('\n');
        text.push_str(&"x\n".repeat(200_000));
        assert!(text.len() < MAX_BATCH_URI_BYTES);

        let result = store.preview_uris(&text).unwrap();

        assert_eq!(result.previews.len(), MAX_QR_SESSIONS);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].line, MAX_QR_SESSIONS + 1);
        assert!(result.errors[0].message.contains("后续"));
        assert_eq!(lock_unpoisoned(&store.imports).len(), MAX_QR_SESSIONS);
    }

    #[test]
    fn oversized_batch_uri_text_is_rejected_before_parsing() {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        store.activate();
        let error = store
            .preview_uris(&"x".repeat(MAX_BATCH_URI_BYTES + 1))
            .unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert!(lock_unpoisoned(&store.imports).is_empty());
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

    #[test]
    fn existing_entries_without_pinned_default_to_unpinned() {
        let entry: StoredEntry = serde_json::from_value(serde_json::json!({
            "id": "existing-entry",
            "name": "Existing",
            "issuer": "PetalDesk",
            "accountName": "account",
            "iconEmoji": "key",
            "algorithm": "sha1",
            "digits": 6,
            "period": 30,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z",
            "secret": [1, 2, 3]
        }))
        .unwrap();

        assert!(!entry.pinned);
    }

    #[cfg(windows)]
    fn test_store() -> (tempfile::TempDir, MfaStore) {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        store.activate();
        store
            .configure_recovery_password(RECOVERY_PASSWORD)
            .unwrap();
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
    fn test_stored_entry(id: String) -> StoredEntry {
        StoredEntry {
            id,
            name: "test".to_string(),
            issuer: String::new(),
            account_name: String::new(),
            icon_emoji: "🔐".to_string(),
            pinned: false,
            algorithm: MfaAlgorithm::Sha1,
            digits: 6,
            period: 30,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
            secret: RFC_SECRET.as_bytes().to_vec(),
        }
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
    fn local_key_metadata_round_trip_preserves_both_platform_wrappers() {
        let (_root, store) = test_store();
        let bytes = std::fs::read(&store.vault_path).unwrap();
        let mut vault = decrypt_envelope_with_recovery(&bytes, RECOVERY_PASSWORD).unwrap();
        let original_dpapi = vault.dpapi_wrapped_key.clone();

        apply_keychain_key_id(&mut vault, "mac-keychain-test-item".to_string());
        apply_dpapi_wrapper(&mut vault, &[0xA7; 48]);
        let rebound_dpapi = vault.dpapi_wrapped_key.clone();
        assert_ne!(rebound_dpapi, original_dpapi);
        assert_eq!(
            vault.keychain_key_id.as_deref(),
            Some("mac-keychain-test-item")
        );

        let serialized = serialize_vault(&vault).unwrap();
        let envelope: VaultEnvelope = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(envelope.dpapi_wrapped_key, rebound_dpapi);
        assert_eq!(
            envelope.keychain_key_id.as_deref(),
            Some("mac-keychain-test-item")
        );

        let recovered = decrypt_envelope_with_recovery(&serialized, RECOVERY_PASSWORD).unwrap();
        assert_eq!(recovered.dpapi_wrapped_key, envelope.dpapi_wrapped_key);
        assert_eq!(recovered.keychain_key_id, envelope.keychain_key_id);
    }

    #[cfg(windows)]
    #[test]
    fn legacy_envelope_without_keychain_metadata_still_opens_locally() {
        let (_root, store) = test_store();
        let bytes = std::fs::read(&store.vault_path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value.as_object_mut().unwrap().remove("keychainKeyId");
        let legacy_bytes = serde_json::to_vec_pretty(&value).unwrap();

        let envelope = parse_envelope(&legacy_bytes).unwrap();
        assert!(envelope.keychain_key_id.is_none());
        assert!(decrypt_envelope_local(&legacy_bytes).is_ok());
    }

    #[test]
    fn recovery_password_policy_and_kdf_bounds_are_enforced() {
        assert_eq!(
            validate_recovery_password("short-pass").unwrap_err().code,
            "mfa_recovery_password_policy"
        );
        assert!(validate_recovery_password("123456789012").is_ok());
        assert_eq!(
            validate_recovery_password(&"x".repeat(RECOVERY_PASSWORD_MAX_BYTES + 1))
                .unwrap_err()
                .code,
            "mfa_recovery_password_policy"
        );

        let key = [7u8; 32];
        let wrapper = wrap_recovery_key(&key, RECOVERY_PASSWORD).unwrap();
        assert_eq!(
            unwrap_recovery_key(&wrapper, RECOVERY_PASSWORD)
                .unwrap()
                .as_slice(),
            key
        );
        let mut hostile = wrapper.clone();
        hostile.memory_kib = RECOVERY_KDF_MAX_MEMORY_KIB + 1;
        assert_eq!(
            unwrap_recovery_key(&hostile, RECOVERY_PASSWORD).unwrap_err(),
            RecoveryUnlockError::InvalidEnvelope
        );
        hostile = wrapper;
        hostile.iterations = RECOVERY_KDF_MAX_ITERATIONS + 1;
        assert_eq!(
            unwrap_recovery_key(&hostile, RECOVERY_PASSWORD).unwrap_err(),
            RecoveryUnlockError::InvalidEnvelope
        );
        hostile = wrap_recovery_key(&key, RECOVERY_PASSWORD).unwrap();
        hostile.memory_kib = RECOVERY_KDF_MIN_MEMORY_KIB - 1;
        assert_eq!(
            unwrap_recovery_key(&hostile, RECOVERY_PASSWORD).unwrap_err(),
            RecoveryUnlockError::InvalidEnvelope
        );
        hostile = wrap_recovery_key(&key, RECOVERY_PASSWORD).unwrap();
        hostile.parallelism = RECOVERY_KDF_MAX_PARALLELISM + 1;
        assert_eq!(
            unwrap_recovery_key(&hostile, RECOVERY_PASSWORD).unwrap_err(),
            RecoveryUnlockError::InvalidEnvelope
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_new_vault_requires_recovery_setup_before_first_write() {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        store.activate();
        let status = store.status().unwrap();
        assert!(status.available);
        assert_eq!(status.recovery_state, MfaRecoveryState::SetupRequired);

        let preview = store
            .preview_uri("otpauth://totp/PetalDesk%3Asetup?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk")
            .unwrap()
            .remove(0);
        let error = store.commit_import(&preview.session_id, "🌸").unwrap_err();
        assert_eq!(error.code, "mfa_recovery_setup_required");
        assert!(!store.vault_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn recovery_password_rebinds_a_copied_vault_to_local_dpapi() {
        let (root, store) = test_store();
        let added = import_public_rfc_entry(&store, "portable");
        let original = std::fs::read(&store.vault_path).unwrap();
        let mut envelope: VaultEnvelope = serde_json::from_slice(&original).unwrap();
        envelope.dpapi_wrapped_key = STANDARD_NO_PAD.encode([0xA5; 64]);
        envelope.keychain_key_id = Some("preserved-mac-keychain-item".to_string());
        let copied = serde_json::to_vec_pretty(&envelope).unwrap();
        atomic_write(&store.vault_path, &copied).unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        let status = reopened.status().unwrap();
        assert!(!status.available);
        assert_eq!(status.recovery_state, MfaRecoveryState::PasswordRequired);

        let before_wrong_password = std::fs::read(&reopened.vault_path).unwrap();
        let error = reopened
            .unlock_with_recovery_password("definitely-wrong-password")
            .unwrap_err();
        assert_eq!(error.code, "mfa_recovery_password_invalid");
        assert_eq!(
            std::fs::read(&reopened.vault_path).unwrap(),
            before_wrong_password
        );

        let status = reopened
            .unlock_with_recovery_password(RECOVERY_PASSWORD)
            .unwrap();
        assert!(status.available);
        assert_eq!(status.recovery_state, MfaRecoveryState::Ready);
        assert_eq!(reopened.list_entries().unwrap()[0].id, added.id);
        let rebound = std::fs::read(&reopened.vault_path).unwrap();
        assert_ne!(rebound, copied);
        assert!(decrypt_envelope_local(&rebound).is_ok());
        let rebound_envelope: VaultEnvelope = serde_json::from_slice(&rebound).unwrap();
        assert_eq!(
            rebound_envelope.keychain_key_id.as_deref(),
            Some("preserved-mac-keychain-item")
        );
        drop(reopened);

        let local_reopen = MfaStore::load(root.path()).unwrap();
        local_reopen.activate();
        assert_eq!(local_reopen.list_entries().unwrap()[0].id, added.id);
        assert!(std::fs::read_dir(&local_reopen.backup_path)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| decrypt_envelope_local(&std::fs::read(entry.path()).unwrap()).is_ok()));
    }

    #[cfg(windows)]
    #[test]
    fn changing_password_rewraps_primary_and_all_retained_backups() {
        const NEW_PASSWORD: &str = "petaldesk-new-recovery-password";
        let (_root, store) = test_store();
        import_public_rfc_entry(&store, "password-change");
        store
            .change_recovery_password(RECOVERY_PASSWORD, NEW_PASSWORD)
            .unwrap();

        let current = std::fs::read(&store.vault_path).unwrap();
        assert!(matches!(
            decrypt_envelope_with_recovery(&current, RECOVERY_PASSWORD),
            Err(RecoveryUnlockError::InvalidPassword)
        ));
        assert!(decrypt_envelope_with_recovery(&current, NEW_PASSWORD).is_ok());
        let backups = std::fs::read_dir(&store.backup_path)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| std::fs::read(entry.path()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert!(backups
            .iter()
            .all(|bytes| decrypt_envelope_with_recovery(bytes, NEW_PASSWORD).is_ok()));
        assert!(backups.iter().all(|bytes| matches!(
            decrypt_envelope_with_recovery(bytes, RECOVERY_PASSWORD),
            Err(RecoveryUnlockError::InvalidPassword)
        )));
    }

    #[cfg(windows)]
    #[test]
    fn changing_password_rejects_an_incorrect_current_password_without_writing() {
        const NEW_PASSWORD: &str = "petaldesk-new-recovery-password";
        let (_root, store) = test_store();
        import_public_rfc_entry(&store, "password-change-auth");
        let primary_before = std::fs::read(&store.vault_path).unwrap();
        let backups_before = std::fs::read_dir(&store.backup_path)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| (entry.file_name(), std::fs::read(entry.path()).unwrap()))
            .collect::<Vec<_>>();

        let missing = store.configure_recovery_password(NEW_PASSWORD).unwrap_err();
        assert_eq!(missing.code, "mfa_recovery_password_invalid");

        let error = store
            .change_recovery_password("definitely-wrong-password", NEW_PASSWORD)
            .unwrap_err();

        assert_eq!(error.code, "mfa_recovery_password_invalid");
        assert_eq!(std::fs::read(&store.vault_path).unwrap(), primary_before);
        let backups_after = std::fs::read_dir(&store.backup_path)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| (entry.file_name(), std::fs::read(entry.path()).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(backups_after, backups_before);
        assert!(decrypt_envelope_with_recovery(&primary_before, RECOVERY_PASSWORD).is_ok());
        assert!(matches!(
            decrypt_envelope_with_recovery(&primary_before, NEW_PASSWORD),
            Err(RecoveryUnlockError::InvalidPassword)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn valid_recovery_key_can_restore_a_payload_from_backup() {
        let (root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "recovery-backup");
        store
            .update_entry(MfaEntryUpdateRequest {
                id: entry.id.clone(),
                name: "backup-snapshot".to_string(),
                issuer: "PetalDesk".to_string(),
                account_name: "recovery-backup".to_string(),
                icon_emoji: "🌸".to_string(),
            })
            .unwrap();

        let bytes = std::fs::read(&store.vault_path).unwrap();
        let mut envelope: VaultEnvelope = serde_json::from_slice(&bytes).unwrap();
        envelope.dpapi_wrapped_key = STANDARD_NO_PAD.encode([0x5A; 64]);
        let mut ciphertext = STANDARD_NO_PAD
            .decode(envelope.ciphertext.as_bytes())
            .unwrap();
        *ciphertext.last_mut().unwrap() ^= 0x80;
        envelope.ciphertext = STANDARD_NO_PAD.encode(ciphertext);
        atomic_write(
            &store.vault_path,
            &serde_json::to_vec_pretty(&envelope).unwrap(),
        )
        .unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        assert_eq!(
            reopened.status().unwrap().recovery_state,
            MfaRecoveryState::PasswordRequired
        );
        let status = reopened
            .unlock_with_recovery_password(RECOVERY_PASSWORD)
            .unwrap();
        assert!(status.recovered_from_backup);
        let entries = reopened.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, entry.id);
    }

    #[cfg(windows)]
    #[test]
    fn structurally_corrupt_primary_can_be_recovered_from_portable_backup() {
        let (root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "structure-backup");
        store
            .update_entry(MfaEntryUpdateRequest {
                id: entry.id.clone(),
                name: "portable-backup".to_string(),
                issuer: "PetalDesk".to_string(),
                account_name: "structure-backup".to_string(),
                icon_emoji: "🌸".to_string(),
            })
            .unwrap();
        for backup in std::fs::read_dir(&store.backup_path)
            .unwrap()
            .filter_map(Result::ok)
        {
            let bytes = std::fs::read(backup.path()).unwrap();
            let mut envelope: VaultEnvelope = serde_json::from_slice(&bytes).unwrap();
            envelope.dpapi_wrapped_key = STANDARD_NO_PAD.encode([0x33; 64]);
            atomic_write(
                &backup.path(),
                &serde_json::to_vec_pretty(&envelope).unwrap(),
            )
            .unwrap();
        }
        atomic_write(&store.vault_path, b"not-a-vault").unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        assert_eq!(
            reopened.status().unwrap().recovery_state,
            MfaRecoveryState::PasswordRequired
        );
        let status = reopened
            .unlock_with_recovery_password(RECOVERY_PASSWORD)
            .unwrap();
        assert!(status.recovered_from_backup);
        assert_eq!(reopened.list_entries().unwrap()[0].id, entry.id);
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
        atomic_write(&store.vault_path, b"{\"schemaVersion\":2}").unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        let entries = reopened.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name.starts_with("backup-name-"));
        assert!(decrypt_envelope_local(&std::fs::read(&reopened.vault_path).unwrap()).is_ok());
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
    fn missing_primary_recovers_an_authenticated_backup() {
        let (root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "missing-primary");
        store
            .update_entry(MfaEntryUpdateRequest {
                id: entry.id,
                name: "creates-backup".to_string(),
                issuer: "PetalDesk".to_string(),
                account_name: "missing-primary".to_string(),
                icon_emoji: "🌸".to_string(),
            })
            .unwrap();
        std::fs::remove_file(&store.vault_path).unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        assert_eq!(reopened.list_entries().unwrap().len(), 1);
        assert!(reopened.vault_path.exists());
        assert!(reopened.status().unwrap().recovered_from_backup);
    }

    #[cfg(windows)]
    #[test]
    fn missing_primary_with_only_invalid_backups_never_creates_an_empty_vault() {
        let root = tempfile::tempdir().unwrap();
        let store = MfaStore::load(root.path()).unwrap();
        atomic_write(
            &store.backup_path.join("vault-invalid.json"),
            b"not a vault",
        )
        .unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        let status = reopened.status().unwrap();

        assert!(!status.available);
        assert!(reopened.list_entries().is_err());
        assert!(!reopened.vault_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn reorder_requires_the_complete_unique_current_id_set() {
        let (_root, store) = test_store();
        let first = import_public_rfc_entry(&store, "order-first");
        let second = import_public_rfc_entry(&store, "order-second");
        let third = import_public_rfc_entry(&store, "order-third");
        let baseline = vec![first.id.clone(), second.id.clone(), third.id.clone()];

        for invalid in [
            vec![first.id.clone(), first.id.clone(), third.id.clone()],
            vec![first.id.clone(), second.id.clone(), "unknown".to_string()],
            vec![first.id.clone(), second.id.clone()],
        ] {
            let error = store.reorder_entries(invalid).unwrap_err();
            assert_eq!(error.code, "invalid_input");
            assert_eq!(
                store
                    .list_entries()
                    .unwrap()
                    .into_iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                baseline
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn pinning_and_reordering_keep_pinned_and_unpinned_groups_stable() {
        let (_root, store) = test_store();
        let first = import_public_rfc_entry(&store, "pin-first");
        let second = import_public_rfc_entry(&store, "pin-second");
        let third = import_public_rfc_entry(&store, "pin-third");

        let entries = store.set_entry_pinned(&second.id, true).unwrap();
        assert_eq!(entries[0].id, second.id);
        assert!(entries[0].pinned);

        let entries = store.set_entry_pinned(&third.id, true).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![third.id.as_str(), second.id.as_str(), first.id.as_str()]
        );

        let entries = store
            .reorder_entries(vec![second.id.clone(), first.id.clone(), third.id.clone()])
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![second.id.as_str(), third.id.as_str(), first.id.as_str()]
        );
        assert!(entries[0].pinned && entries[1].pinned && !entries[2].pinned);

        let entries = store.set_entry_pinned(&second.id, false).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![third.id.as_str(), second.id.as_str(), first.id.as_str()]
        );
        assert!(entries[0].pinned && !entries[1].pinned && !entries[2].pinned);
    }

    #[cfg(windows)]
    #[test]
    fn mfa_order_and_pinned_state_persist_after_reopen() {
        let (root, store) = test_store();
        let first = import_public_rfc_entry(&store, "persist-first");
        let second = import_public_rfc_entry(&store, "persist-second");
        let third = import_public_rfc_entry(&store, "persist-third");
        store.set_entry_pinned(&second.id, true).unwrap();
        store
            .reorder_entries(vec![third.id.clone(), second.id.clone(), first.id.clone()])
            .unwrap();
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        let entries = reopened.list_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![second.id.as_str(), third.id.as_str(), first.id.as_str()]
        );
        assert!(entries[0].pinned);
        assert!(!entries[1].pinned && !entries[2].pinned);
    }

    #[cfg(windows)]
    #[test]
    fn reorder_and_pin_roll_back_when_persistence_fails() {
        let (_root, store) = test_store();
        let first = import_public_rfc_entry(&store, "order-rollback-first");
        let second = import_public_rfc_entry(&store, "order-rollback-second");
        let baseline = vec![first.id.clone(), second.id.clone()];
        std::fs::remove_file(&store.vault_path).unwrap();
        std::fs::create_dir(&store.vault_path).unwrap();

        assert!(store
            .reorder_entries(vec![second.id.clone(), first.id.clone()])
            .is_err());
        let entries = store.list_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            baseline
        );
        assert!(entries.iter().all(|entry| !entry.pinned));

        assert!(store.set_entry_pinned(&second.id, true).is_err());
        let entries = store.list_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            baseline
        );
        assert!(entries.iter().all(|entry| !entry.pinned));
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
        assert!(store.list_trash().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn external_vault_replacement_is_not_overwritten() {
        let (_root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "external-conflict");
        let external = std::fs::read(&store.vault_path).unwrap();
        atomic_write(&store.vault_path, b"externally-replaced-vault").unwrap();

        let error = store
            .update_entry(MfaEntryUpdateRequest {
                id: entry.id,
                name: "must-not-overwrite".to_string(),
                issuer: "PetalDesk".to_string(),
                account_name: "external-conflict".to_string(),
                icon_emoji: "🔐".to_string(),
            })
            .unwrap_err();

        assert_eq!(error.code, "mfa_vault_conflict");
        assert_eq!(
            std::fs::read(&store.vault_path).unwrap(),
            b"externally-replaced-vault"
        );
        let conflicts = std::fs::read_dir(&store.conflict_path)
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(conflicts.len(), 1);
        let conflict_bytes = std::fs::read(conflicts[0].path()).unwrap();
        assert_ne!(conflict_bytes, external);
        assert!(decrypt_envelope_local(&conflict_bytes).is_ok());
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
    fn authenticated_entry_export_round_trips_through_uri_and_qr() {
        let (_root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "export-round-trip");

        let exported = store.export_entry(&entry.id, RECOVERY_PASSWORD).unwrap();

        assert_eq!(exported.id, entry.id);
        assert_eq!(exported.name, entry.name);
        assert_eq!(exported.secret_base32, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        let parsed = parse_otpauth_uri(&exported.otpauth_uri).unwrap();
        assert_eq!(parsed.entry.issuer, entry.issuer);
        assert_eq!(parsed.entry.account_name, entry.account_name);
        assert_eq!(parsed.entry.algorithm, entry.algorithm);
        assert_eq!(parsed.entry.digits, entry.digits);
        assert_eq!(parsed.entry.period, entry.period);
        assert_eq!(parsed.entry.secret, RFC_SECRET.as_bytes());

        let encoded_png = exported
            .qr_png_data_url
            .strip_prefix("data:image/png;base64,")
            .unwrap();
        let mut png = STANDARD.decode(encoded_png).unwrap();
        let mut gray = ImageReader::new(Cursor::new(&png))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap()
            .to_luma8();
        let payloads = decode_qr_payloads(gray.width(), gray.height(), gray.as_raw()).unwrap();
        assert!(payloads
            .iter()
            .any(|payload| payload.as_slice() == exported.otpauth_uri.as_bytes()));
        gray.as_mut().fill(0);
        png.zeroize();

        let debug = format!("{exported:?}");
        assert!(!debug.contains(&exported.secret_base32));
        assert!(!debug.contains(&exported.otpauth_uri));
    }

    #[cfg(windows)]
    #[test]
    fn deleted_entries_are_encrypted_in_trash_and_can_be_restored_or_purged() {
        let (root, store) = test_store();
        let first = import_public_rfc_entry(&store, "trash-first");
        let second = import_public_rfc_entry(&store, "trash-second");

        store.delete_entry(&first.id).unwrap();
        assert_eq!(
            store
                .list_entries()
                .unwrap()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![second.id.as_str()]
        );
        let trash = store.list_trash().unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].id, first.id);
        assert!(!trash[0].deleted_at.is_empty());
        assert!(!serde_json::to_string(&trash).unwrap().contains("secret"));
        drop(store);

        let reopened = MfaStore::load(root.path()).unwrap();
        reopened.activate();
        assert_eq!(reopened.list_trash().unwrap()[0].id, first.id);
        let restored = reopened.restore_entry(&first.id).unwrap();
        assert_eq!(restored.id, first.id);
        assert!(reopened.list_trash().unwrap().is_empty());
        assert!(reopened
            .list_entries()
            .unwrap()
            .iter()
            .any(|entry| entry.id == first.id));

        reopened.delete_entry(&first.id).unwrap();
        reopened.permanently_delete_entry(&first.id).unwrap();
        assert!(reopened.list_trash().unwrap().is_empty());
        assert!(reopened.restore_entry(&first.id).is_err());

        reopened.delete_entry(&second.id).unwrap();
        assert_eq!(reopened.list_trash().unwrap().len(), 1);
        reopened.empty_trash().unwrap();
        assert!(reopened.list_trash().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn trash_mutations_roll_back_exactly_when_persistence_conflicts() {
        let (_root, store) = test_store();
        let first = import_public_rfc_entry(&store, "trash-rollback-first");
        let second = import_public_rfc_entry(&store, "trash-rollback-second");
        store.delete_entry(&first.id).unwrap();
        store.delete_entry(&second.id).unwrap();

        let original_vault = std::fs::read(&store.vault_path).unwrap();
        let snapshot = || {
            let runtime = lock_unpoisoned(&store.runtime);
            serde_json::to_vec(&runtime.vault.as_ref().unwrap().payload).unwrap()
        };
        let expected_payload = snapshot();

        atomic_write(&store.vault_path, b"restore-conflict").unwrap();
        let restore_error = store.restore_entry(&first.id).unwrap_err();
        assert_eq!(restore_error.code, "mfa_vault_conflict");
        assert_eq!(snapshot(), expected_payload);

        atomic_write(&store.vault_path, &original_vault).unwrap();
        atomic_write(&store.vault_path, b"permanent-delete-conflict").unwrap();
        let delete_error = store.permanently_delete_entry(&first.id).unwrap_err();
        assert_eq!(delete_error.code, "mfa_vault_conflict");
        assert_eq!(snapshot(), expected_payload);

        atomic_write(&store.vault_path, &original_vault).unwrap();
        atomic_write(&store.vault_path, b"empty-trash-conflict").unwrap();
        let empty_error = store.empty_trash().unwrap_err();
        assert_eq!(empty_error.code, "mfa_vault_conflict");
        assert_eq!(snapshot(), expected_payload);
    }

    #[test]
    fn trash_timestamps_are_bounded_rfc3339_values() {
        assert!(is_valid_vault_timestamp("2026-08-02T12:34:56.789Z"));
        assert!(!is_valid_vault_timestamp(""));
        assert!(!is_valid_vault_timestamp("not-a-timestamp"));
        assert!(!is_valid_vault_timestamp(
            &"2".repeat(MAX_TIMESTAMP_BYTES + 1)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn legacy_vault_payload_is_upgraded_without_losing_entries() {
        let (_root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "legacy-schema");
        let mut bytes = std::fs::read(&store.vault_path).unwrap();
        let mut vault = decrypt_envelope_local(&bytes).unwrap();
        vault.payload.schema_version = LEGACY_VAULT_SCHEMA_VERSION;
        bytes = serialize_vault(&vault).unwrap();
        let mut envelope: VaultEnvelope = serde_json::from_slice(&bytes).unwrap();
        envelope.schema_version = LEGACY_VAULT_SCHEMA_VERSION;
        bytes = serde_json::to_vec(&envelope).unwrap();

        let migrated = decrypt_envelope_local(&bytes).unwrap();

        assert_eq!(migrated.payload.schema_version, VAULT_SCHEMA_VERSION);
        assert!(migrated.payload.trash.is_empty());
        assert_eq!(migrated.payload.entries.len(), 1);
        assert_eq!(migrated.payload.entries[0].id, entry.id);
    }

    #[cfg(windows)]
    #[test]
    fn entry_export_requires_the_current_password_before_entry_lookup() {
        let (_root, store) = test_store();
        let entry = import_public_rfc_entry(&store, "export-auth");
        let wrong_password = "definitely-wrong-password";

        let wrong = store.export_entry(&entry.id, wrong_password).unwrap_err();
        assert_eq!(wrong.code, "mfa_recovery_password_invalid");
        assert!(!wrong.to_string().contains(wrong_password));

        let unknown = store
            .export_entry("missing-entry", RECOVERY_PASSWORD)
            .unwrap_err();
        assert_eq!(unknown.code, "not_found");

        let unknown_without_auth = store
            .export_entry("missing-entry", wrong_password)
            .unwrap_err();
        assert_eq!(unknown_without_auth.code, "mfa_recovery_password_invalid");

        const NEW_PASSWORD: &str = "petaldesk-export-new-recovery-password";
        store
            .change_recovery_password(RECOVERY_PASSWORD, NEW_PASSWORD)
            .unwrap();
        assert_eq!(
            store
                .export_entry(&entry.id, RECOVERY_PASSWORD)
                .unwrap_err()
                .code,
            "mfa_recovery_password_invalid"
        );
        assert_eq!(
            store.export_entry(&entry.id, NEW_PASSWORD).unwrap().id,
            entry.id
        );
    }

    #[cfg(windows)]
    #[test]
    fn entry_export_preserves_escaped_parameters_and_algorithm_settings() {
        let (_root, store) = test_store();
        let name = "工作验证器";
        let issuer = "研发 / Cloud & Co";
        let account = "alice+tag@example.com / 中文";
        let preview = store
            .preview_manual(MfaManualImportRequest {
                name: name.to_string(),
                issuer: issuer.to_string(),
                account_name: account.to_string(),
                secret: SensitiveText("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_string()),
                icon_emoji: "🛡️".to_string(),
                algorithm: MfaAlgorithm::Sha512,
                digits: 8,
                period: 45,
            })
            .unwrap()
            .remove(0);
        let saved = store
            .commit_import(&preview.session_id, preview.icon_emoji.as_deref().unwrap())
            .unwrap();

        let exported = store.export_entry(&saved.id, RECOVERY_PASSWORD).unwrap();

        assert_eq!(exported.name, name);
        assert_eq!(exported.issuer, issuer);
        assert_eq!(exported.account_name, account);
        assert_eq!(exported.icon_emoji, "🛡️");
        assert!(exported.otpauth_uri.contains("%2F"));
        assert!(exported.otpauth_uri.contains("%26"));
        let parsed_url = url::Url::parse(&exported.otpauth_uri).unwrap();
        let parameters = parsed_url
            .query_pairs()
            .into_owned()
            .collect::<HashMap<_, _>>();
        assert_eq!(parameters.get("issuer").map(String::as_str), Some(issuer));
        assert_eq!(
            parameters.get("algorithm").map(String::as_str),
            Some("SHA512")
        );
        assert_eq!(parameters.get("digits").map(String::as_str), Some("8"));
        assert_eq!(parameters.get("period").map(String::as_str), Some("45"));

        let parsed = parse_otpauth_uri(&exported.otpauth_uri).unwrap();
        assert_eq!(parsed.entry.issuer, issuer);
        assert_eq!(parsed.entry.account_name, account);
        assert_eq!(parsed.entry.algorithm, MfaAlgorithm::Sha512);
        assert_eq!(parsed.entry.digits, 8);
        assert_eq!(parsed.entry.period, 45);
    }

    #[cfg(windows)]
    #[test]
    fn batch_import_commits_all_accounts_with_one_vault_update() {
        let (_root, store) = test_store();
        let input = [
            "otpauth://totp/PetalDesk%3Abatch-one?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk",
            "otpauth://totp/PetalDesk%3Abatch-two?secret=JBSWY3DPEHPK3PXP&issuer=PetalDesk",
        ]
        .join("\n");
        let result = store.preview_uris(&input).unwrap();
        assert!(result.errors.is_empty());
        let requests = result
            .previews
            .iter()
            .map(|preview| MfaImportCommitRequest {
                session_id: preview.session_id.clone(),
                icon_emoji: preview.icon_emoji.clone().unwrap(),
            })
            .collect();

        let saved = store.commit_imports(requests).unwrap();

        assert_eq!(saved.len(), 2);
        assert_eq!(store.list_entries().unwrap().len(), 2);
        assert!(lock_unpoisoned(&store.imports).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn batch_import_validation_does_not_consume_any_session() {
        let (_root, store) = test_store();
        let result = store
            .preview_uris(
                "otpauth://totp/PetalDesk%3Avalid-one?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk\n\
                 otpauth://totp/PetalDesk%3Avalid-two?secret=JBSWY3DPEHPK3PXP&issuer=PetalDesk",
            )
            .unwrap();
        let valid_request = MfaImportCommitRequest {
            session_id: result.previews[0].session_id.clone(),
            icon_emoji: "🌸".to_string(),
        };
        let error = store
            .commit_imports(vec![
                valid_request,
                MfaImportCommitRequest {
                    session_id: "missing-session".to_string(),
                    icon_emoji: "🔐".to_string(),
                },
            ])
            .unwrap_err();

        assert_eq!(error.code, "not_found");
        assert_eq!(lock_unpoisoned(&store.imports).len(), 2);
        assert!(store.list_entries().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn batch_import_restores_every_session_when_persistence_fails() {
        let (_root, store) = test_store();
        let original_vault = std::fs::read(&store.vault_path).unwrap();
        let result = store
            .preview_uris(
                "otpauth://totp/PetalDesk%3Arollback-one?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk\n\
                 otpauth://totp/PetalDesk%3Arollback-two?secret=JBSWY3DPEHPK3PXP&issuer=PetalDesk",
            )
            .unwrap();
        {
            let mut imports = lock_unpoisoned(&store.imports);
            for (index, preview) in result.previews.iter().enumerate() {
                let pending = imports.get_mut(&preview.session_id).unwrap();
                pending.entry.icon_emoji = format!("original-{index}");
                pending.entry.updated_at = format!("2026-01-0{}T00:00:00.000Z", index + 1);
            }
        }
        let original_sessions = {
            let imports = lock_unpoisoned(&store.imports);
            result
                .previews
                .iter()
                .map(|preview| {
                    let pending = imports.get(&preview.session_id).unwrap();
                    (
                        preview.session_id.clone(),
                        pending.entry.icon_emoji.clone(),
                        pending.entry.updated_at.clone(),
                        pending.expires_at,
                    )
                })
                .collect::<Vec<_>>()
        };
        let requests = result
            .previews
            .iter()
            .enumerate()
            .map(|(index, preview)| MfaImportCommitRequest {
                session_id: preview.session_id.clone(),
                icon_emoji: ["⭐", "☁️"][index].to_string(),
            })
            .collect::<Vec<_>>();
        atomic_write(&store.vault_path, b"externally-replaced-vault").unwrap();

        let error = store.commit_imports(requests.clone()).unwrap_err();

        assert_eq!(error.code, "mfa_vault_conflict");
        assert!(store.list_entries().unwrap().is_empty());
        {
            let imports = lock_unpoisoned(&store.imports);
            assert_eq!(imports.len(), 2);
            for (session_id, icon, updated_at, expires_at) in &original_sessions {
                let pending = imports.get(session_id).unwrap();
                assert_eq!(&pending.entry.icon_emoji, icon);
                assert_eq!(&pending.entry.updated_at, updated_at);
                assert_eq!(&pending.expires_at, expires_at);
            }
        }
        assert_eq!(
            std::fs::read(&store.vault_path).unwrap(),
            b"externally-replaced-vault"
        );

        atomic_write(&store.vault_path, &original_vault).unwrap();
        let saved = store.commit_imports(requests).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].icon_emoji, "⭐");
        assert_eq!(saved[1].icon_emoji, "☁️");
        assert_eq!(store.list_entries().unwrap().len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn single_import_save_failure_restores_the_original_preview() {
        let (_root, store) = test_store();
        let original_vault = std::fs::read(&store.vault_path).unwrap();
        let preview = store
            .preview_uri("otpauth://totp/PetalDesk%3Asingle-rollback?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk")
            .unwrap()
            .remove(0);
        let original_expiry = {
            let mut imports = lock_unpoisoned(&store.imports);
            let pending = imports.get_mut(&preview.session_id).unwrap();
            pending.entry.icon_emoji = "original-icon".to_string();
            pending.entry.updated_at = "2026-01-01T00:00:00.000Z".to_string();
            pending.expires_at
        };
        atomic_write(&store.vault_path, b"externally-replaced-vault").unwrap();

        let error = store.commit_import(&preview.session_id, "⭐").unwrap_err();

        assert_eq!(error.code, "mfa_vault_conflict");
        assert!(store.list_entries().unwrap().is_empty());
        {
            let imports = lock_unpoisoned(&store.imports);
            let pending = imports.get(&preview.session_id).unwrap();
            assert_eq!(pending.entry.icon_emoji, "original-icon");
            assert_eq!(pending.entry.updated_at, "2026-01-01T00:00:00.000Z");
            assert_eq!(pending.expires_at, original_expiry);
        }

        atomic_write(&store.vault_path, &original_vault).unwrap();
        let saved = store.commit_import(&preview.session_id, "⭐").unwrap();
        assert_eq!(saved.icon_emoji, "⭐");
    }

    #[cfg(windows)]
    #[test]
    fn batch_import_capacity_counts_trash_without_consuming_previews() {
        let (_root, store) = test_store();
        let result = store
            .preview_uris(
                "otpauth://totp/PetalDesk%3Acapacity-one?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk\n\
                 otpauth://totp/PetalDesk%3Acapacity-two?secret=JBSWY3DPEHPK3PXP&issuer=PetalDesk",
            )
            .unwrap();
        let requests = result
            .previews
            .iter()
            .map(|preview| MfaImportCommitRequest {
                session_id: preview.session_id.clone(),
                icon_emoji: preview.icon_emoji.clone().unwrap(),
            })
            .collect::<Vec<_>>();
        {
            let mut runtime = lock_unpoisoned(&store.runtime);
            store.ensure_unlocked(&mut runtime).unwrap();
            let vault = runtime.vault.as_mut().unwrap();
            vault.payload.trash = (0..MAX_VAULT_ENTRIES - 1)
                .map(|index| TrashedEntry {
                    deleted_at: "2026-01-01T00:00:00.000Z".to_string(),
                    entry: test_stored_entry(format!("trash-{index}")),
                })
                .collect();
        }

        let error = store.commit_imports(requests).unwrap_err();

        assert_eq!(error.code, "mfa_vault_entry_limit");
        assert_eq!(lock_unpoisoned(&store.imports).len(), 2);
        let runtime = lock_unpoisoned(&store.runtime);
        let vault = runtime.vault.as_ref().unwrap();
        assert!(vault.payload.entries.is_empty());
        assert_eq!(vault.payload.trash.len(), MAX_VAULT_ENTRIES - 1);
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
                .map(|index| test_stored_entry(format!("entry-{index}")))
                .collect();
        }
        let preview = store
            .preview_uri("otpauth://totp/PetalDesk%3Alimit?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=PetalDesk")
            .unwrap()
            .remove(0);
        let error = store.commit_import(&preview.session_id, "🔐").unwrap_err();
        assert_eq!(error.code, "mfa_vault_entry_limit");
        assert_eq!(store.list_entries().unwrap().len(), MAX_VAULT_ENTRIES);
        assert!(lock_unpoisoned(&store.imports).contains_key(&preview.session_id));
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
