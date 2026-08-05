//! Encrypted local password vault.
//!
//! Passwords are encrypted with an independent XChaCha20-Poly1305 data key.
//! On Windows that key is bound to the current user with DPAPI and is also
//! wrapped by the user's recovery password. Public list/status structures
//! deliberately never contain a password.

use crate::error::{AppError, AppResult};
use crate::storage::{
    atomic_write, atomic_write_json, ensure_managed_subdirectory, INTERNAL_DATA_DIR,
};
use argon2::{Algorithm as Argon2Algorithm, Argon2, Params as Argon2Params, Version};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
#[cfg(windows)]
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, WebviewWindow};
use url::Url;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const PASSWORDS_DIR: &str = "passwords";
const VAULT_FILE: &str = "vault.json";
const SETTINGS_FILE: &str = "settings.json";
const BACKUP_DIR: &str = "backups";
const CONFLICT_DIR: &str = "conflicts";
const VAULT_SCHEMA_VERSION: u32 = 1;
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const VAULT_AAD: &[u8] = b"PetalDesk password vault v1";
const RECOVERY_KEY_AAD: &[u8] = b"PetalDesk password recovery key v1";
#[cfg(windows)]
const DPAPI_ENTROPY: &[u8] = b"PetalDesk password DPAPI wrapper v1";
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
const MAX_VAULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_VAULT_ENTRIES: usize = 10_000;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_SITE_NAME_BYTES: usize = 512;
const MAX_LOGIN_URL_BYTES: usize = 4096;
const MAX_ORIGIN_BYTES: usize = 1024;
const MAX_USERNAME_BYTES: usize = 4096;
const MAX_PASSWORD_BYTES: usize = 16 * 1024;
const MAX_NOTES_BYTES: usize = 16 * 1024;
const MAX_TEMPLATE_ID_BYTES: usize = 256;
const MAX_TEMPLATE_LABEL_BYTES: usize = 512;
const MAX_TEMPLATE_SELECTOR_BYTES: usize = 512;
const MAX_TEMPLATE_SELECTORS: usize = 16;
const BACKUP_LIMIT: usize = 5;
const CONFLICT_LIMIT: usize = 10;
const REVEAL_TTL: Duration = Duration::from_secs(15);
const CLIPBOARD_TTL: Duration = Duration::from_secs(30);
#[cfg(windows)]
const CLIPBOARD_RETRY_COUNT: usize = 10;
#[cfg(windows)]
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(20);

fn generic_vault_error() -> AppError {
    AppError::new(
        "password_vault_unavailable",
        "密码保险库损坏或暂时无法读取；不会创建空白保险库。",
    )
}

fn recovery_password_required_error() -> AppError {
    AppError::new(
        "password_recovery_password_required",
        "此密码保险库来自另一台电脑，请输入恢复密码完成迁移。",
    )
}

fn recovery_setup_required_error() -> AppError {
    AppError::new(
        "password_recovery_setup_required",
        "请先设置恢复密码，再添加或修改站点账户。",
    )
}

fn invalid_recovery_password_error() -> AppError {
    AppError::new(
        "password_recovery_password_invalid",
        "恢复密码不正确，请重新输入。",
    )
}

fn session_closed_error() -> AppError {
    AppError::new(
        "password_session_closed",
        "密码管理器已锁定，请重新打开后再操作。",
    )
}

fn manually_locked_error() -> AppError {
    AppError::new(
        "password_vault_locked",
        "密码保险库已显式锁定，请输入恢复密码解锁。",
    )
}

/// A deserializable string whose value is redacted from Debug and wiped on
/// drop. Use this for every IPC input that can contain credentials.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct SensitiveText(String);

impl SensitiveText {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn new(value: String) -> Self {
        Self(value)
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

/// A short-lived serializable secret. Tauri serializes it to the requesting
/// webview and the Rust allocation is wiped immediately afterwards.
#[derive(Serialize)]
#[serde(transparent)]
pub struct SensitiveValue(String);

impl SensitiveValue {
    #[cfg(test)]
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SensitiveValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveValue(<redacted>)")
    }
}

impl Drop for SensitiveValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PasswordRecoveryState {
    SetupRequired,
    Ready,
    PasswordRequired,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordStatus {
    pub available: bool,
    pub locked: bool,
    pub entry_count: usize,
    pub protection: String,
    pub recovery_state: PasswordRecoveryState,
    /// During password-vault setup, true means MFA already has the global
    /// recovery password and this vault must reuse it. It remains true after
    /// the password vault itself is configured.
    pub shared_recovery_configured: bool,
    pub capture_configured: bool,
    pub capture_enabled: bool,
    pub session_epoch: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovered_from_backup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PasswordEntrySummary {
    pub id: String,
    pub site_name: String,
    pub login_url: String,
    pub origin: String,
    pub username: String,
    pub notes: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    pub allow_insecure_http: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordEntryInput {
    pub site_name: String,
    pub login_url: String,
    pub username: SensitiveText,
    pub password: SensitiveText,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub allow_insecure_http: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordEntryUpdateInput {
    pub id: String,
    pub site_name: String,
    pub login_url: String,
    pub username: SensitiveText,
    #[serde(default)]
    pub password: Option<SensitiveText>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub allow_insecure_http: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PasswordTemplateMode {
    Password,
    TwoStep,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasswordTemplateDefinition {
    pub id: String,
    pub label: String,
    pub version: u32,
    pub mode: PasswordTemplateMode,
    pub origin: String,
    pub username_selectors: Vec<String>,
    pub password_selectors: Vec<String>,
}

impl Drop for PasswordTemplateDefinition {
    fn drop(&mut self) {
        self.id.zeroize();
        self.label.zeroize();
        self.origin.zeroize();
        self.username_selectors.zeroize();
        self.password_selectors.zeroize();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordRevealResult {
    pub id: String,
    pub password: SensitiveValue,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PasswordCaptureAction {
    Disabled,
    NoPrompt,
    Create,
    Update,
    SelectAccount,
    UsernameRequired,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PasswordCaptureAccount {
    pub entry_id: String,
    pub site_name: String,
    pub username: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordCaptureDecision {
    pub action: PasswordCaptureAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub origin: String,
    pub insecure_http: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_choices: Vec<PasswordCaptureAccount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordCaptureCandidate {
    pub origin: String,
    pub username: SensitiveText,
    pub password: SensitiveText,
    #[serde(default)]
    pub allow_insecure_http: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordGeneratorOptions {
    #[serde(default = "default_generated_password_length")]
    pub length: usize,
    #[serde(default = "default_true")]
    pub uppercase: bool,
    #[serde(default = "default_true")]
    pub lowercase: bool,
    #[serde(default = "default_true")]
    pub digits: bool,
    #[serde(default = "default_true")]
    pub symbols: bool,
    #[serde(default = "default_true")]
    pub exclude_ambiguous: bool,
}

impl Default for PasswordGeneratorOptions {
    fn default() -> Self {
        Self {
            length: default_generated_password_length(),
            uppercase: true,
            lowercase: true,
            digits: true,
            symbols: true,
            exclude_ambiguous: true,
        }
    }
}

fn default_generated_password_length() -> usize {
    20
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedPassword {
    pub password: SensitiveValue,
}

pub(crate) struct BrowserFillData {
    pub entry_id: String,
    pub login_url: String,
    pub origin: String,
    pub username: String,
    pub password: String,
    pub user_template: Option<PasswordTemplateDefinition>,
    pub allow_insecure_http: bool,
}

impl Drop for BrowserFillData {
    fn drop(&mut self) {
        self.username.zeroize();
        self.password.zeroize();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordSettings {
    #[serde(default = "settings_schema_version")]
    schema_version: u32,
    #[serde(default)]
    capture_configured: bool,
    #[serde(default)]
    capture_enabled: bool,
}

impl Default for PasswordSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            capture_configured: false,
            capture_enabled: false,
        }
    }
}

fn settings_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultEnvelope {
    schema_version: u32,
    dpapi_wrapped_key: String,
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
    entries: Vec<StoredPasswordEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPasswordEntry {
    id: String,
    site_name: String,
    login_url: String,
    origin: String,
    username: String,
    password: String,
    notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_template: Option<PasswordTemplateDefinition>,
    allow_insecure_http: bool,
    created_at: String,
    updated_at: String,
}

impl Drop for StoredPasswordEntry {
    fn drop(&mut self) {
        self.site_name.zeroize();
        self.login_url.zeroize();
        self.origin.zeroize();
        self.username.zeroize();
        self.password.zeroize();
        self.notes.zeroize();
        if let Some(template_id) = self.template_id.as_mut() {
            template_id.zeroize();
        }
    }
}

struct UnlockedVault {
    payload: VaultPayload,
    key: Zeroizing<Vec<u8>>,
    dpapi_wrapped_key: String,
    recovery_wrapped_key: Option<RecoveryKeyEnvelope>,
    disk_hash: Option<String>,
}

impl Drop for UnlockedVault {
    fn drop(&mut self) {
        self.dpapi_wrapped_key.zeroize();
    }
}

struct RuntimeState {
    vault: Option<UnlockedVault>,
    recovery_state: PasswordRecoveryState,
    recovered_from_backup: bool,
    manually_locked: bool,
    locked_entry_count: usize,
}

struct SessionState {
    epoch: AtomicU64,
    active: AtomicBool,
}

struct ClipboardLease {
    #[cfg(windows)]
    sequence: u32,
    marker: Vec<u8>,
    clear_at: Instant,
}

impl Drop for ClipboardLease {
    fn drop(&mut self) {
        self.marker.zeroize();
    }
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

#[derive(Debug)]
struct ValidatedLoginUrl {
    login_url: String,
    origin: String,
    allow_insecure_http: bool,
}

#[cfg(windows)]
fn local_protection_label(state: PasswordRecoveryState) -> &'static str {
    match state {
        PasswordRecoveryState::SetupRequired => "windows-dpapi",
        PasswordRecoveryState::Ready | PasswordRecoveryState::PasswordRequired => {
            "windows-dpapi-recovery-password"
        }
        PasswordRecoveryState::Unavailable => "unavailable",
    }
}

#[cfg(not(windows))]
fn local_protection_label(_state: PasswordRecoveryState) -> &'static str {
    "unavailable"
}

/// Password vault state managed by Tauri. Call `activate` when the password
/// manager window opens and `lock` when it closes.
pub struct PasswordStore {
    vault_path: PathBuf,
    settings_path: PathBuf,
    backup_path: PathBuf,
    conflict_path: PathBuf,
    recovery_transaction_path: PathBuf,
    runtime: Arc<Mutex<RuntimeState>>,
    settings: Arc<Mutex<PasswordSettings>>,
    session: Arc<SessionState>,
    lifecycle_lock: Arc<Mutex<()>>,
    clipboard: Arc<Mutex<Option<ClipboardLease>>>,
}

impl PasswordStore {
    pub fn load(data_storage_path: &Path) -> AppResult<Self> {
        let root = ensure_managed_subdirectory(
            data_storage_path,
            &[INTERNAL_DATA_DIR, "tools", PASSWORDS_DIR],
        )?;
        let backup_path = ensure_managed_subdirectory(
            data_storage_path,
            &[INTERNAL_DATA_DIR, "tools", PASSWORDS_DIR, BACKUP_DIR],
        )?;
        let conflict_path = ensure_managed_subdirectory(
            data_storage_path,
            &[INTERNAL_DATA_DIR, "tools", PASSWORDS_DIR, CONFLICT_DIR],
        )?;
        let recovery_transaction_path = ensure_managed_subdirectory(
            data_storage_path,
            &[INTERNAL_DATA_DIR, "tools", "recovery"],
        )?;
        let settings_path = root.join(SETTINGS_FILE);
        let settings = load_settings(&settings_path)?;
        let vault_path = root.join(VAULT_FILE);
        let recovery_state = if vault_path.exists() {
            if read_envelope(&vault_path).is_ok() {
                PasswordRecoveryState::Ready
            } else {
                PasswordRecoveryState::Unavailable
            }
        } else {
            PasswordRecoveryState::SetupRequired
        };
        let store = Self {
            vault_path,
            settings_path,
            backup_path,
            conflict_path,
            recovery_transaction_path,
            runtime: Arc::new(Mutex::new(RuntimeState {
                vault: None,
                recovery_state,
                recovered_from_backup: false,
                manually_locked: false,
                locked_entry_count: 0,
            })),
            settings: Arc::new(Mutex::new(settings)),
            session: Arc::new(SessionState {
                epoch: AtomicU64::new(0),
                active: AtomicBool::new(false),
            }),
            lifecycle_lock: Arc::new(Mutex::new(())),
            clipboard: Arc::new(Mutex::new(None)),
        };
        Ok(store)
    }

    /// Opens or refocuses one logical password-manager session.
    pub fn activate(&self) -> u64 {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        if self.session.active.load(Ordering::Acquire) {
            return self.session.epoch.load(Ordering::Acquire);
        }
        let epoch = self.session.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.session.active.store(true, Ordering::Release);
        epoch
    }

    /// Invalidates queued work before clearing decrypted data.
    pub fn deactivate(&self) -> u64 {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.session.active.store(false, Ordering::Release);
        self.session.epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn clear_deactivated_state(&self, epoch: u64) {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        if self.session.active.load(Ordering::Acquire)
            || self.session.epoch.load(Ordering::Acquire) != epoch
        {
            return;
        }
        let mut runtime = lock_unpoisoned(&self.runtime);
        if let Some(vault) = runtime.vault.take() {
            runtime.locked_entry_count = vault.payload.entries.len();
        }
        runtime.manually_locked = false;
        drop(runtime);
        force_expire_clipboard(&self.clipboard);
        if !clear_clipboard_now(&self.clipboard) {
            schedule_clipboard_cleanup(Arc::downgrade(&self.clipboard), Instant::now());
        }
    }

    pub fn lock(&self) {
        let epoch = self.deactivate();
        self.clear_deactivated_state(epoch);
    }

    /// Explicitly locks decrypted credentials while preserving the current
    /// password-manager window session. The epoch changes first so queued
    /// work from before the lock cannot publish a result afterwards.
    pub(crate) fn lock_current_session(&self) -> AppResult<()> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        if !self.session.active.load(Ordering::Acquire) {
            return Err(session_closed_error());
        }
        self.session.epoch.fetch_add(1, Ordering::AcqRel);
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.locked_entry_count = runtime
            .vault
            .as_ref()
            .map_or(0, |vault| vault.payload.entries.len());
        runtime.vault = None;
        runtime.manually_locked = self.vault_path.exists();
        runtime.recovered_from_backup = false;
        drop(runtime);
        force_expire_clipboard(&self.clipboard);
        if !clear_clipboard_now(&self.clipboard) {
            schedule_clipboard_cleanup(Arc::downgrade(&self.clipboard), Instant::now());
        }
        Ok(())
    }

    pub(crate) fn require_active_epoch(&self) -> AppResult<u64> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        if !self.session.active.load(Ordering::Acquire) {
            return Err(session_closed_error());
        }
        let epoch = self.session.epoch.load(Ordering::Acquire);
        if !self.session.active.load(Ordering::Acquire)
            || self.session.epoch.load(Ordering::Acquire) != epoch
        {
            return Err(session_closed_error());
        }
        Ok(epoch)
    }

    fn validate_epoch(&self, epoch: u64) -> AppResult<()> {
        if !self.session.active.load(Ordering::Acquire)
            || self.session.epoch.load(Ordering::Acquire) != epoch
        {
            return Err(session_closed_error());
        }
        Ok(())
    }

    #[cfg(test)]
    fn status(&self) -> AppResult<PasswordStatus> {
        let epoch = self.require_active_epoch()?;
        self.status_at(epoch)
    }

    pub(crate) fn status_at(&self, epoch: u64) -> AppResult<PasswordStatus> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        #[cfg(all(not(windows), not(test)))]
        {
            let settings = lock_unpoisoned(&self.settings).clone();
            return Ok(PasswordStatus {
                available: false,
                locked: true,
                entry_count: 0,
                protection: "unavailable".to_string(),
                recovery_state: PasswordRecoveryState::Unavailable,
                shared_recovery_configured: false,
                capture_configured: settings.capture_configured,
                capture_enabled: false,
                session_epoch: epoch,
                recovered_from_backup: false,
                message: Some("密码管理器首版仅支持 Windows。".to_string()),
            });
        }
        let mut runtime = lock_unpoisoned(&self.runtime);
        let manually_locked = runtime.manually_locked;
        let available = manually_locked || self.ensure_unlocked(&mut runtime).is_ok();
        let (locked, entry_count) = if manually_locked {
            (true, runtime.locked_entry_count)
        } else {
            runtime
                .vault
                .as_ref()
                .map(|vault| (false, vault.payload.entries.len()))
                .unwrap_or((self.vault_path.exists(), 0))
        };
        let settings = lock_unpoisoned(&self.settings).clone();
        Ok(PasswordStatus {
            available,
            locked,
            entry_count,
            protection: local_protection_label(runtime.recovery_state).to_string(),
            recovery_state: runtime.recovery_state,
            shared_recovery_configured: matches!(
                runtime.recovery_state,
                PasswordRecoveryState::Ready | PasswordRecoveryState::PasswordRequired
            ),
            capture_configured: settings.capture_configured,
            capture_enabled: settings.capture_enabled,
            session_epoch: epoch,
            recovered_from_backup: runtime.recovered_from_backup,
            message: if manually_locked {
                Some("密码保险库已锁定，请输入恢复密码解锁。".to_string())
            } else {
                match runtime.recovery_state {
                    PasswordRecoveryState::SetupRequired => {
                        Some("请先设置恢复密码，之后即可保存站点账户。".to_string())
                    }
                    PasswordRecoveryState::Ready if runtime.recovered_from_backup => {
                        Some("密码主保险库缺失或损坏，已从最近的有效备份恢复。".to_string())
                    }
                    PasswordRecoveryState::PasswordRequired => Some(
                        "此保险库缺少当前系统可用的本机密钥，请输入恢复密码完成迁移。".to_string(),
                    ),
                    PasswordRecoveryState::Unavailable => {
                        Some("密码数据当前无法读取；不会创建空白保险库。".to_string())
                    }
                    PasswordRecoveryState::Ready => None,
                }
            },
        })
    }

    #[cfg(test)]
    fn list_entries(&self) -> AppResult<Vec<PasswordEntrySummary>> {
        let epoch = self.require_active_epoch()?;
        self.list_entries_at(epoch)
    }

    pub(crate) fn list_entries_at(&self, epoch: u64) -> AppResult<Vec<PasswordEntrySummary>> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;
        Ok(vault.payload.entries.iter().map(entry_summary).collect())
    }

    pub(crate) fn browser_fill_data(&self, entry_id: &str) -> AppResult<BrowserFillData> {
        let epoch = self.require_active_epoch()?;
        validate_entry_id(entry_id)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let entry = runtime
            .vault
            .as_ref()
            .and_then(|vault| {
                vault
                    .payload
                    .entries
                    .iter()
                    .find(|entry| entry.id == entry_id)
            })
            .ok_or_else(|| AppError::not_found("没有找到这个站点账户。"))?;
        Ok(BrowserFillData {
            entry_id: entry.id.clone(),
            login_url: entry.login_url.clone(),
            origin: entry.origin.clone(),
            username: entry.username.clone(),
            password: entry.password.clone(),
            user_template: entry.user_template.clone(),
            allow_insecure_http: entry.allow_insecure_http,
        })
    }

    #[cfg(test)]
    fn create_entry(&self, input: PasswordEntryInput) -> AppResult<PasswordEntrySummary> {
        let epoch = self.require_active_epoch()?;
        self.create_entry_at(input, epoch)
    }

    pub(crate) fn create_entry_at(
        &self,
        input: PasswordEntryInput,
        epoch: u64,
    ) -> AppResult<PasswordEntrySummary> {
        let site_name = validate_site_name(&input.site_name)?;
        validate_username(input.username.as_str())?;
        validate_password(input.password.as_str())?;
        let notes = validate_notes(&input.notes)?;
        let template_id = validate_template_id(input.template_id.as_deref())?;
        let login = validate_login_url(&input.login_url, input.allow_insecure_http)?;
        let now = vault_timestamp();
        let entry = StoredPasswordEntry {
            id: Uuid::new_v4().to_string(),
            site_name,
            login_url: login.login_url,
            origin: login.origin,
            username: input.username.as_str().to_owned(),
            password: input.password.as_str().to_owned(),
            notes,
            template_id,
            user_template: None,
            allow_insecure_http: login.allow_insecure_http,
            created_at: now.clone(),
            updated_at: now,
        };

        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        if vault.payload.entries.len() >= MAX_VAULT_ENTRIES {
            return Err(AppError::new(
                "password_vault_too_large",
                "密码保险库的账户数量已达到上限。",
            ));
        }
        ensure_unique_account(&vault.payload.entries, &entry.origin, &entry.username, None)?;
        let summary = entry_summary(&entry);
        vault.payload.entries.push(entry);
        if let Err(error) = self.save_vault(vault) {
            let _ = vault.payload.entries.pop();
            return Err(error);
        }
        Ok(summary)
    }

    #[cfg(test)]
    fn update_entry(&self, input: PasswordEntryUpdateInput) -> AppResult<PasswordEntrySummary> {
        let epoch = self.require_active_epoch()?;
        self.update_entry_at(input, epoch)
    }

    pub(crate) fn update_entry_at(
        &self,
        input: PasswordEntryUpdateInput,
        epoch: u64,
    ) -> AppResult<PasswordEntrySummary> {
        validate_entry_id(&input.id)?;
        let site_name = validate_site_name(&input.site_name)?;
        validate_username(input.username.as_str())?;
        if let Some(password) = input.password.as_ref() {
            validate_password(password.as_str())?;
        }
        let notes = validate_notes(&input.notes)?;
        let template_id = validate_template_id(input.template_id.as_deref())?;
        let login = validate_login_url(&input.login_url, input.allow_insecure_http)?;

        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let index = vault
            .payload
            .entries
            .iter()
            .position(|entry| entry.id == input.id)
            .ok_or_else(|| AppError::not_found("没有找到这个站点账户。"))?;
        ensure_unique_account(
            &vault.payload.entries,
            &login.origin,
            input.username.as_str(),
            Some(&input.id),
        )?;
        let existing = &vault.payload.entries[index];
        let password = input
            .password
            .as_ref()
            .map(|value| value.as_str().to_owned())
            .unwrap_or_else(|| existing.password.clone());
        let user_template = existing.user_template.as_ref().and_then(|template| {
            (template_id.as_deref() == Some(template.id.as_str())
                && login.origin == existing.origin)
                .then(|| template.clone())
        });
        let replacement = StoredPasswordEntry {
            id: existing.id.clone(),
            site_name,
            login_url: login.login_url,
            origin: login.origin,
            username: input.username.as_str().to_owned(),
            password,
            notes,
            template_id,
            user_template,
            allow_insecure_http: login.allow_insecure_http,
            created_at: existing.created_at.clone(),
            updated_at: vault_timestamp(),
        };
        let result = entry_summary(&replacement);
        let previous = std::mem::replace(&mut vault.payload.entries[index], replacement);
        if let Err(error) = self.save_vault(vault) {
            vault.payload.entries[index] = previous;
            return Err(error);
        }
        Ok(result)
    }

    pub(crate) fn set_recorded_template_at(
        &self,
        entry_id: &str,
        mut template: PasswordTemplateDefinition,
        epoch: u64,
    ) -> AppResult<PasswordEntrySummary> {
        validate_entry_id(entry_id)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let entry = vault
            .payload
            .entries
            .iter_mut()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个站点账户。"))?;

        template.id = format!("user-recorded:{entry_id}");
        validate_template_definition(&template, &entry.origin, entry.allow_insecure_http)?;
        let previous_template_id = entry.template_id.clone();
        let previous_template = entry.user_template.clone();
        let previous_updated_at = entry.updated_at.clone();
        entry.template_id = Some(template.id.clone());
        entry.user_template = Some(template);
        entry.updated_at = vault_timestamp();
        let result = entry_summary(entry);
        if let Err(error) = self.save_vault(vault) {
            let entry = vault
                .payload
                .entries
                .iter_mut()
                .find(|entry| entry.id == entry_id)
                .ok_or_else(generic_vault_error)?;
            entry.template_id = previous_template_id;
            entry.user_template = previous_template;
            entry.updated_at = previous_updated_at;
            return Err(error);
        }
        Ok(result)
    }

    #[cfg(test)]
    fn delete_entry(&self, entry_id: &str) -> AppResult<()> {
        let epoch = self.require_active_epoch()?;
        self.delete_entry_at(entry_id, epoch)
    }

    pub(crate) fn delete_entry_at(&self, entry_id: &str, epoch: u64) -> AppResult<()> {
        validate_entry_id(entry_id)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let vault = runtime.vault.as_mut().ok_or_else(generic_vault_error)?;
        let index = vault
            .payload
            .entries
            .iter()
            .position(|entry| entry.id == entry_id)
            .ok_or_else(|| AppError::not_found("没有找到这个站点账户。"))?;
        let removed = vault.payload.entries.remove(index);
        if let Err(error) = self.save_vault(vault) {
            vault.payload.entries.insert(index, removed);
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    fn reveal_password(&self, entry_id: &str) -> AppResult<PasswordRevealResult> {
        let epoch = self.require_active_epoch()?;
        self.reveal_password_at(entry_id, epoch)
    }

    pub(crate) fn reveal_password_at(
        &self,
        entry_id: &str,
        epoch: u64,
    ) -> AppResult<PasswordRevealResult> {
        validate_entry_id(entry_id)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let entry = runtime
            .vault
            .as_ref()
            .and_then(|vault| {
                vault
                    .payload
                    .entries
                    .iter()
                    .find(|entry| entry.id == entry_id)
            })
            .ok_or_else(|| AppError::not_found("没有找到这个站点账户。"))?;
        Ok(PasswordRevealResult {
            id: entry.id.clone(),
            password: SensitiveValue(entry.password.clone()),
            expires_at: unix_millis().saturating_add(REVEAL_TTL.as_millis() as u64),
        })
    }

    pub(crate) fn copy_field_at(
        &self,
        entry_id: &str,
        password: bool,
        epoch: u64,
    ) -> AppResult<()> {
        validate_entry_id(entry_id)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let entry = runtime
            .vault
            .as_ref()
            .and_then(|vault| {
                vault
                    .payload
                    .entries
                    .iter()
                    .find(|entry| entry.id == entry_id)
            })
            .ok_or_else(|| AppError::not_found("没有找到这个站点账户。"))?;
        let value = Zeroizing::new(if password {
            entry.password.clone()
        } else {
            entry.username.clone()
        });
        drop(runtime);
        write_sensitive_clipboard(&value, &self.clipboard)
    }

    #[cfg(test)]
    fn capture_decision(
        &self,
        candidate: PasswordCaptureCandidate,
    ) -> AppResult<PasswordCaptureDecision> {
        let epoch = self.require_active_epoch()?;
        self.capture_decision_at(candidate, epoch)
    }

    pub(crate) fn capture_decision_at(
        &self,
        candidate: PasswordCaptureCandidate,
        epoch: u64,
    ) -> AppResult<PasswordCaptureDecision> {
        let capture_enabled = lock_unpoisoned(&self.settings).capture_enabled;
        if !capture_enabled {
            return Ok(PasswordCaptureDecision {
                action: PasswordCaptureAction::Disabled,
                entry_id: None,
                origin: String::new(),
                insecure_http: false,
                account_choices: Vec::new(),
            });
        }
        validate_password(candidate.password.as_str())?;
        let (origin, insecure_http) =
            validate_exact_origin(&candidate.origin, candidate.allow_insecure_http)?;

        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
        let vault = runtime.vault.as_ref().ok_or_else(generic_vault_error)?;

        // Password-change pages occasionally expose a password field without
        // the account name.  Do not guess which account to mutate: return a
        // bounded list of same-origin accounts so the browser can ask the user
        // to choose one.  A single account is handled as a normal update by
        // the browser service; an empty list is explicitly rejected.
        if candidate.username.as_str().is_empty() {
            let account_choices = vault
                .payload
                .entries
                .iter()
                .filter(|entry| entry.origin == origin)
                .map(|entry| PasswordCaptureAccount {
                    entry_id: entry.id.clone(),
                    site_name: entry.site_name.clone(),
                    username: entry.username.clone(),
                })
                .collect::<Vec<_>>();
            return Ok(PasswordCaptureDecision {
                action: if account_choices.is_empty() {
                    PasswordCaptureAction::UsernameRequired
                } else if account_choices.len() == 1 {
                    PasswordCaptureAction::Update
                } else {
                    PasswordCaptureAction::SelectAccount
                },
                entry_id: (account_choices.len() == 1).then(|| account_choices[0].entry_id.clone()),
                origin,
                insecure_http,
                account_choices,
            });
        }

        validate_username(candidate.username.as_str())?;
        if let Some(entry) = vault.payload.entries.iter().find(|entry| {
            entry.origin == origin
                && constant_time_eq(
                    entry.username.as_bytes(),
                    candidate.username.as_str().as_bytes(),
                )
        }) {
            let action = if constant_time_eq(
                entry.password.as_bytes(),
                candidate.password.as_str().as_bytes(),
            ) {
                PasswordCaptureAction::NoPrompt
            } else {
                PasswordCaptureAction::Update
            };
            return Ok(PasswordCaptureDecision {
                action,
                entry_id: Some(entry.id.clone()),
                origin,
                insecure_http,
                account_choices: Vec::new(),
            });
        }
        Ok(PasswordCaptureDecision {
            action: PasswordCaptureAction::Create,
            entry_id: None,
            origin,
            insecure_http,
            account_choices: Vec::new(),
        })
    }

    #[cfg(test)]
    fn set_capture_enabled(&self, enabled: bool) -> AppResult<PasswordStatus> {
        let epoch = self.require_active_epoch()?;
        self.set_capture_enabled_at(enabled, epoch)
    }

    pub(crate) fn set_capture_enabled_at(
        &self,
        enabled: bool,
        epoch: u64,
    ) -> AppResult<PasswordStatus> {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut settings = lock_unpoisoned(&self.settings);
        let previous = settings.clone();
        settings.capture_configured = true;
        settings.capture_enabled = enabled;
        if let Err(error) = atomic_write_json(&self.settings_path, &*settings) {
            *settings = previous;
            return Err(error);
        }
        drop(settings);
        drop(_lifecycle);
        self.status_at(epoch)
    }

    pub(crate) fn shared_recovery_transaction_lock(&self) -> MutexGuard<'_, ()> {
        lock_unpoisoned(&self.lifecycle_lock)
    }

    pub(crate) fn shared_recovery_is_configured_locked(&self) -> AppResult<bool> {
        if lock_unpoisoned(&self.runtime)
            .vault
            .as_ref()
            .is_some_and(|vault| vault.recovery_wrapped_key.is_some())
        {
            return Ok(true);
        }
        if self.vault_path.exists() {
            read_envelope(&self.vault_path)?;
            return Ok(true);
        }
        if self.backup_candidates_exist() {
            return Err(generic_vault_error());
        }
        Ok(false)
    }

    pub(crate) fn shared_recovery_vault_path(&self) -> &Path {
        &self.vault_path
    }

    pub(crate) fn shared_recovery_backup_path(&self) -> &Path {
        &self.backup_path
    }

    pub(crate) fn shared_recovery_transaction_path(&self) -> &Path {
        &self.recovery_transaction_path
    }

    pub(crate) fn shared_recovery_snapshot_locked(&self) -> AppResult<Option<Vec<u8>>> {
        if self.vault_path.exists() {
            read_bounded_vault_bytes(&self.vault_path).map(Some)
        } else if self.backup_candidates_exist() {
            Err(generic_vault_error())
        } else {
            Ok(None)
        }
    }

    pub(crate) fn refresh_after_shared_recovery_restore(&self) {
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.vault = None;
        runtime.recovered_from_backup = false;
        runtime.recovery_state = if self.vault_path.exists() {
            if read_envelope(&self.vault_path).is_ok() {
                PasswordRecoveryState::Ready
            } else {
                PasswordRecoveryState::Unavailable
            }
        } else {
            runtime.locked_entry_count = 0;
            runtime.manually_locked = false;
            PasswordRecoveryState::SetupRequired
        };
    }

    #[cfg(test)]
    pub(crate) fn verify_shared_recovery_password(&self, password: &str) -> AppResult<()> {
        validate_recovery_password(password)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.verify_shared_recovery_password_locked(password)
    }

    pub(crate) fn verify_shared_recovery_password_locked(&self, password: &str) -> AppResult<()> {
        validate_recovery_password(password)?;
        let runtime = lock_unpoisoned(&self.runtime);
        if let Some(vault) = runtime.vault.as_ref() {
            return verify_current_recovery_password(vault, password);
        }
        drop(runtime);
        if !self.vault_path.exists() {
            return Err(recovery_setup_required_error());
        }
        let bytes = read_bounded_vault_bytes(&self.vault_path)?;
        decrypt_envelope_with_recovery(&bytes, password)
            .map(|_| ())
            .map_err(|error| match error {
                RecoveryUnlockError::InvalidPassword => invalid_recovery_password_error(),
                RecoveryUnlockError::InvalidEnvelope | RecoveryUnlockError::InvalidPayload => {
                    generic_vault_error()
                }
            })
    }

    pub(crate) fn configure_shared_recovery_password_locked(
        &self,
        password: &str,
        current_password: Option<&str>,
    ) -> AppResult<()> {
        validate_recovery_password(password)?;
        let original = if self.vault_path.exists() {
            Some(read_bounded_vault_bytes(&self.vault_path)?)
        } else {
            if self.backup_candidates_exist() {
                return Err(generic_vault_error());
            }
            None
        };
        let mut vault = match original.as_deref() {
            Some(bytes) => decrypt_envelope_with_recovery(
                bytes,
                current_password.ok_or_else(invalid_recovery_password_error)?,
            )
            .map_err(|error| match error {
                RecoveryUnlockError::InvalidPassword => invalid_recovery_password_error(),
                RecoveryUnlockError::InvalidEnvelope | RecoveryUnlockError::InvalidPayload => {
                    generic_vault_error()
                }
            })?,
            None if current_password.is_none() => new_empty_vault()?,
            None => return Err(recovery_setup_required_error()),
        };
        vault.recovery_wrapped_key = Some(wrap_recovery_key(&vault.key, password)?);
        let replacement = serialize_vault(&vault)?;
        let original_hash = original.as_deref().map(bytes_hash);
        if !self.disk_matches(original_hash.as_deref()) {
            return Err(vault_conflict_error());
        }
        self.rotate_backup()?;
        if !self.disk_matches(original_hash.as_deref()) {
            return Err(vault_conflict_error());
        }
        atomic_write(&self.vault_path, &replacement)?;
        if let Err(error) = self.reset_backups_to_current(&replacement) {
            let rollback = match original.as_deref() {
                Some(bytes) => atomic_write(&self.vault_path, bytes)
                    .and_then(|_| self.reset_backups_to_current(bytes)),
                None => self.rollback_initial_recovery_write(),
            };
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(AppError::new(
                    "password_recovery_rollback_failed",
                    "恢复密码更新失败，且密码保险库无法自动回滚，请立即保留数据目录并重试。",
                )
                .with_details(serde_json::json!({
                    "updateError": error.message,
                    "rollbackError": rollback_error.message,
                }))),
            };
        }
        vault.disk_hash = Some(bytes_hash(&replacement));
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.recovery_state = PasswordRecoveryState::Ready;
        runtime.recovered_from_backup = false;
        if self.session.active.load(Ordering::Acquire) && !runtime.manually_locked {
            runtime.vault = Some(vault);
        } else {
            runtime.vault = None;
        }
        Ok(())
    }

    fn rollback_initial_recovery_write(&self) -> AppResult<()> {
        if self.vault_path.exists() {
            std::fs::remove_file(&self.vault_path)
                .map_err(|error| AppError::io("回滚密码保险库", error))?;
        }
        for path in backup_files_newest_first(&self.backup_path)? {
            std::fs::remove_file(&path)
                .map_err(|error| AppError::io("回滚密码保险库备份", error))?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn configure_recovery_password(
        &self,
        password: &str,
        current_password: Option<&str>,
    ) -> AppResult<PasswordStatus> {
        let epoch = self.require_active_epoch()?;
        self.configure_recovery_password_at(password, current_password, epoch)
    }

    #[cfg(test)]
    pub(crate) fn configure_recovery_password_at(
        &self,
        password: &str,
        current_password: Option<&str>,
        epoch: u64,
    ) -> AppResult<PasswordStatus> {
        validate_recovery_password(password)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        self.ensure_unlocked(&mut runtime)?;
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
                "password_vault_conflict",
                "设置恢复密码后保险库又被外部修改，请重新打开并确认数据。",
            ));
        }
        self.reset_backups_to_current(&current)?;
        runtime.recovery_state = PasswordRecoveryState::Ready;
        runtime.recovered_from_backup = false;
        drop(runtime);
        drop(_lifecycle);
        self.status_at(epoch)
    }

    #[cfg(test)]
    fn unlock_with_recovery_password(&self, password: &str) -> AppResult<PasswordStatus> {
        let epoch = self.require_active_epoch()?;
        self.unlock_with_recovery_password_at(password, epoch)
    }

    pub(crate) fn unlock_with_recovery_password_at(
        &self,
        password: &str,
        epoch: u64,
    ) -> AppResult<PasswordStatus> {
        validate_recovery_password(password)?;
        let _lifecycle = lock_unpoisoned(&self.lifecycle_lock);
        self.validate_epoch(epoch)?;
        let mut runtime = lock_unpoisoned(&self.runtime);
        if let Some(vault) = runtime.vault.as_ref() {
            verify_current_recovery_password(vault, password)?;
            runtime.recovery_state = PasswordRecoveryState::Ready;
            runtime.manually_locked = false;
            runtime.locked_entry_count = 0;
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
                Err(RecoveryUnlockError::InvalidPassword) => {
                    return Err(invalid_recovery_password_error())
                }
                Err(RecoveryUnlockError::InvalidEnvelope | RecoveryUnlockError::InvalidPayload) => {
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
        rebind_local_key(&mut vault)?;
        let rebound = serialize_vault(&vault)?;
        if let Some(expected) = primary.as_deref() {
            let current = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
            if bytes_hash(&current) != bytes_hash(expected) {
                return Err(vault_conflict_error());
            }
            if preserve_primary {
                self.preserve_corrupt_bytes(expected)?;
            } else {
                self.rotate_backup()?;
            }
            let current = std::fs::read(&self.vault_path).map_err(|_| generic_vault_error())?;
            if bytes_hash(&current) != bytes_hash(expected) {
                return Err(vault_conflict_error());
            }
        } else if self.vault_path.exists() {
            return Err(vault_conflict_error());
        }
        atomic_write(&self.vault_path, &rebound)?;
        self.reset_backups_to_current(&rebound)?;
        vault.disk_hash = Some(bytes_hash(&rebound));
        runtime.vault = Some(vault);
        runtime.recovery_state = PasswordRecoveryState::Ready;
        runtime.recovered_from_backup = recovered_from_backup;
        runtime.manually_locked = false;
        runtime.locked_entry_count = 0;
        drop(runtime);
        drop(_lifecycle);
        self.status_at(epoch)
    }

    fn ensure_unlocked(&self, runtime: &mut RuntimeState) -> AppResult<()> {
        if runtime.manually_locked {
            return Err(manually_locked_error());
        }
        if runtime.vault.is_some() {
            return Ok(());
        }
        if !self.vault_path.exists() {
            match self.find_valid_backup_local() {
                LocalBackupResult::Found(bytes, mut vault) => {
                    atomic_write(&self.vault_path, &bytes)?;
                    vault.disk_hash = Some(bytes_hash(&bytes));
                    runtime.recovery_state = PasswordRecoveryState::Ready;
                    runtime.recovered_from_backup = true;
                    runtime.vault = Some(vault);
                    return Ok(());
                }
                LocalBackupResult::PasswordRequired => {
                    runtime.recovery_state = PasswordRecoveryState::PasswordRequired;
                    return Err(recovery_password_required_error());
                }
                LocalBackupResult::None if self.backup_candidates_exist() => {
                    runtime.recovery_state = PasswordRecoveryState::Unavailable;
                    return Err(generic_vault_error());
                }
                LocalBackupResult::None => {}
            }
            runtime.recovery_state = PasswordRecoveryState::SetupRequired;
            runtime.vault = Some(new_empty_vault()?);
            return Ok(());
        }
        let primary = match read_bounded_vault_bytes(&self.vault_path) {
            Ok(bytes) => bytes,
            Err(_) => match self.find_valid_backup_local() {
                LocalBackupResult::Found(bytes, mut vault) => {
                    atomic_write(&self.vault_path, &bytes)?;
                    vault.disk_hash = Some(bytes_hash(&bytes));
                    runtime.recovery_state = PasswordRecoveryState::Ready;
                    runtime.recovered_from_backup = true;
                    runtime.vault = Some(vault);
                    return Ok(());
                }
                LocalBackupResult::PasswordRequired => {
                    runtime.recovery_state = PasswordRecoveryState::PasswordRequired;
                    return Err(recovery_password_required_error());
                }
                LocalBackupResult::None => {
                    runtime.recovery_state = PasswordRecoveryState::Unavailable;
                    return Err(generic_vault_error());
                }
            },
        };
        match decrypt_envelope_local(&primary) {
            Ok(mut vault) => {
                vault.disk_hash = Some(bytes_hash(&primary));
                runtime.recovery_state = PasswordRecoveryState::Ready;
                runtime.vault = Some(vault);
                Ok(())
            }
            Err(LocalUnlockError::LocalKeyUnavailable) => {
                runtime.recovery_state = PasswordRecoveryState::PasswordRequired;
                Err(recovery_password_required_error())
            }
            Err(LocalUnlockError::InvalidEnvelope | LocalUnlockError::InvalidPayload) => {
                match self.find_valid_backup_local() {
                    LocalBackupResult::Found(bytes, mut vault) => {
                        self.preserve_corrupt_bytes(&primary)?;
                        atomic_write(&self.vault_path, &bytes)?;
                        vault.disk_hash = Some(bytes_hash(&bytes));
                        runtime.recovery_state = PasswordRecoveryState::Ready;
                        runtime.recovered_from_backup = true;
                        runtime.vault = Some(vault);
                        Ok(())
                    }
                    LocalBackupResult::PasswordRequired => {
                        runtime.recovery_state = PasswordRecoveryState::PasswordRequired;
                        Err(recovery_password_required_error())
                    }
                    LocalBackupResult::None => {
                        runtime.recovery_state = PasswordRecoveryState::Unavailable;
                        Err(generic_vault_error())
                    }
                }
            }
        }
    }

    fn find_valid_backup_local(&self) -> LocalBackupResult {
        let Ok(mut files) = backup_files_newest_first(&self.backup_path) else {
            return LocalBackupResult::None;
        };
        let mut password_required = false;
        for file in files.drain(..) {
            let Ok(bytes) = read_bounded_vault_bytes(&file) else {
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
        for file in backup_files_newest_first(&self.backup_path)? {
            let Ok(bytes) = read_bounded_vault_bytes(&file) else {
                continue;
            };
            if let Ok(vault) = decrypt_envelope_with_recovery(&bytes, password) {
                return Ok(Some((bytes, vault)));
            }
        }
        Ok(None)
    }

    fn backup_candidates_exist(&self) -> bool {
        std::fs::read_dir(&self.backup_path).map_or(true, |entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry.path().extension().is_some_and(|ext| ext == "json")
            })
        })
    }

    fn save_vault(&self, vault: &mut UnlockedVault) -> AppResult<()> {
        let bytes = serialize_vault(vault)?;
        if !self.disk_matches(vault.disk_hash.as_deref()) {
            let conflict = self.write_conflict(&bytes)?;
            return Err(conflict_write_error(&conflict));
        }
        self.rotate_backup()?;
        if !self.disk_matches(vault.disk_hash.as_deref()) {
            let conflict = self.write_conflict(&bytes)?;
            return Err(conflict_write_error(&conflict));
        }
        atomic_write(&self.vault_path, &bytes)?;
        vault.disk_hash = Some(bytes_hash(&bytes));
        Ok(())
    }

    fn disk_matches(&self, expected: Option<&str>) -> bool {
        match expected {
            Some(expected) => std::fs::read(&self.vault_path)
                .ok()
                .is_some_and(|current| bytes_hash(&current) == expected),
            None => !self.vault_path.exists(),
        }
    }

    fn rotate_backup(&self) -> AppResult<()> {
        if !self.vault_path.exists() {
            return Ok(());
        }
        let previous = read_bounded_vault_bytes(&self.vault_path)?;
        let file = self.backup_path.join(snapshot_file_name("vault"));
        atomic_write(&file, &previous)?;
        trim_json_files(&self.backup_path, BACKUP_LIMIT, "读取密码备份目录")
    }

    fn reset_backups_to_current(&self, bytes: &[u8]) -> AppResult<()> {
        let current = self.backup_path.join(snapshot_file_name("vault"));
        atomic_write(&current, bytes)?;
        for entry in std::fs::read_dir(&self.backup_path)
            .map_err(|error| AppError::io("读取密码备份目录", error))?
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path != current && path.extension().is_some_and(|ext| ext == "json") {
                if std::fs::remove_file(&path).is_err() {
                    atomic_write(&path, bytes)?;
                }
            }
        }
        let expected = bytes_hash(bytes);
        for path in backup_files_newest_first(&self.backup_path)? {
            if bytes_hash(&read_bounded_vault_bytes(&path)?) != expected {
                return Err(vault_conflict_error());
            }
        }
        Ok(())
    }

    fn preserve_corrupt_bytes(&self, bytes: &[u8]) -> AppResult<()> {
        let target = self.conflict_path.join(snapshot_file_name("corrupt-vault"));
        atomic_write(&target, bytes)?;
        trim_json_files(&self.conflict_path, CONFLICT_LIMIT, "读取密码冲突目录")
    }

    fn write_conflict(&self, bytes: &[u8]) -> AppResult<PathBuf> {
        let target = self.conflict_path.join(snapshot_file_name("vault"));
        atomic_write(&target, bytes)?;
        trim_json_files(&self.conflict_path, CONFLICT_LIMIT, "读取密码冲突目录")?;
        Ok(target)
    }
}

fn load_settings(path: &Path) -> AppResult<PasswordSettings> {
    if !path.exists() {
        let settings = PasswordSettings::default();
        atomic_write_json(path, &settings)?;
        return Ok(settings);
    }
    let valid = std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_SETTINGS_BYTES)
        .and_then(|_| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<PasswordSettings>(&bytes).ok())
        .filter(|settings| settings.schema_version == SETTINGS_SCHEMA_VERSION);
    if let Some(settings) = valid {
        return Ok(settings);
    }
    preserve_invalid_settings(path);
    let settings = PasswordSettings::default();
    atomic_write_json(path, &settings)?;
    Ok(settings)
}

fn preserve_invalid_settings(path: &Path) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let target = path.with_file_name(format!(
        "settings-corrupt-{}-{}.json",
        Utc::now().format("%Y%m%d%H%M%S%3f"),
        Uuid::new_v4()
    ));
    let _ = atomic_write(&target, &bytes);
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn snapshot_file_name(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}.json",
        Utc::now().format("%Y%m%d%H%M%S%3f"),
        Uuid::new_v4()
    )
}

fn backup_files_newest_first(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = std::fs::read_dir(directory)
        .map_err(|error| AppError::io("读取密码备份目录", error))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.path().extension().is_some_and(|ext| ext == "json")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    files.sort_by_key(|path| std::cmp::Reverse(path.file_name().map(|name| name.to_owned())));
    Ok(files)
}

fn trim_json_files(directory: &Path, keep: usize, action: &str) -> AppResult<()> {
    let mut files = std::fs::read_dir(directory)
        .map_err(|error| AppError::io(action, error))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.path().extension().is_some_and(|ext| ext == "json")
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for old in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(old.path());
    }
    Ok(())
}

fn vault_conflict_error() -> AppError {
    AppError::new(
        "password_vault_conflict",
        "密码保险库已被外部修改，请重新打开并确认数据。",
    )
}

fn conflict_write_error(path: &Path) -> AppError {
    AppError::new(
        "password_vault_conflict",
        "密码保险库已被外部修改；本次更改已加密保存到冲突目录，未覆盖现有数据。",
    )
    .with_details(serde_json::json!({
        "conflictPath": path.to_string_lossy(),
    }))
}

fn new_empty_vault() -> AppResult<UnlockedVault> {
    let mut key = Zeroizing::new(vec![0u8; 32]);
    getrandom::fill(&mut key).map_err(|_| generic_vault_error())?;
    let dpapi_wrapped_key = STANDARD_NO_PAD.encode(protect_local_key(&key)?);
    Ok(UnlockedVault {
        payload: VaultPayload {
            schema_version: VAULT_SCHEMA_VERSION,
            entries: Vec::new(),
        },
        key,
        dpapi_wrapped_key,
        recovery_wrapped_key: None,
        disk_hash: None,
    })
}

fn rebind_local_key(vault: &mut UnlockedVault) -> AppResult<()> {
    let wrapped = protect_local_key(&vault.key)?;
    vault.dpapi_wrapped_key.zeroize();
    vault.dpapi_wrapped_key = STANDARD_NO_PAD.encode(wrapped);
    Ok(())
}

fn read_envelope(path: &Path) -> AppResult<VaultEnvelope> {
    let bytes = read_bounded_vault_bytes(path)?;
    parse_envelope(&bytes).map_err(|_| generic_vault_error())
}

fn read_bounded_vault_bytes(path: &Path) -> AppResult<Vec<u8>> {
    let metadata = std::fs::metadata(path).map_err(|_| generic_vault_error())?;
    if !metadata.is_file() || metadata.len() > MAX_VAULT_BYTES as u64 {
        return Err(generic_vault_error());
    }
    let bytes = std::fs::read(path).map_err(|_| generic_vault_error())?;
    if bytes.len() > MAX_VAULT_BYTES {
        return Err(generic_vault_error());
    }
    Ok(bytes)
}

fn parse_envelope(bytes: &[u8]) -> Result<VaultEnvelope, LocalUnlockError> {
    if bytes.is_empty() || bytes.len() > MAX_VAULT_BYTES {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    let envelope: VaultEnvelope =
        serde_json::from_slice(bytes).map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    if envelope.schema_version != VAULT_SCHEMA_VERSION
        || envelope.dpapi_wrapped_key.is_empty()
        || envelope.dpapi_wrapped_key.len() > 8192
    {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    let wrapped = STANDARD_NO_PAD
        .decode(envelope.dpapi_wrapped_key.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    let nonce = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    if wrapped.is_empty() || wrapped.len() > 4096 || nonce.len() != 24 || ciphertext.is_empty() {
        return Err(LocalUnlockError::InvalidEnvelope);
    }
    validate_recovery_key_envelope(&envelope.recovery_wrapped_key)
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    Ok(envelope)
}

fn decrypt_envelope_local(bytes: &[u8]) -> Result<UnlockedVault, LocalUnlockError> {
    let envelope = parse_envelope(bytes)?;
    let wrapped = STANDARD_NO_PAD
        .decode(envelope.dpapi_wrapped_key.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    let nonce = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| LocalUnlockError::InvalidEnvelope)?;
    let key = unprotect_local_key(&wrapped).map_err(|_| LocalUnlockError::LocalKeyUnavailable)?;
    if key.len() != 32 {
        return Err(LocalUnlockError::LocalKeyUnavailable);
    }
    let payload = decrypt_vault_payload(&key, &nonce, &ciphertext)
        .map_err(|_| LocalUnlockError::InvalidPayload)?;
    Ok(UnlockedVault {
        payload,
        key,
        dpapi_wrapped_key: envelope.dpapi_wrapped_key,
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
    let nonce = STANDARD_NO_PAD
        .decode(envelope.nonce.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let ciphertext = STANDARD_NO_PAD
        .decode(envelope.ciphertext.as_bytes())
        .map_err(|_| RecoveryUnlockError::InvalidEnvelope)?;
    let payload = decrypt_vault_payload(&key, &nonce, &ciphertext)
        .map_err(|_| RecoveryUnlockError::InvalidPayload)?;
    Ok(UnlockedVault {
        payload,
        key,
        dpapi_wrapped_key: envelope.dpapi_wrapped_key,
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
    let nonce_array: [u8; 24] = nonce_bytes.try_into().map_err(|_| generic_vault_error())?;
    let nonce: XNonce = nonce_array.into();
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| generic_vault_error())?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| generic_vault_error())?,
    );
    let payload: VaultPayload =
        serde_json::from_slice(&plaintext).map_err(|_| generic_vault_error())?;
    validate_payload(&payload)?;
    Ok(payload)
}

fn serialize_vault(vault: &UnlockedVault) -> AppResult<Vec<u8>> {
    let recovery_wrapped_key = vault
        .recovery_wrapped_key
        .clone()
        .ok_or_else(recovery_setup_required_error)?;
    if vault.key.len() != 32 || vault.dpapi_wrapped_key.is_empty() {
        return Err(generic_vault_error());
    }
    validate_payload(&vault.payload)?;
    let mut plaintext =
        Zeroizing::new(serde_json::to_vec(&vault.payload).map_err(|_| generic_vault_error())?);
    if plaintext.len() > MAX_VAULT_BYTES {
        return Err(AppError::new(
            "password_vault_too_large",
            "密码保险库超过安全大小限制。",
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
        recovery_wrapped_key,
        nonce: STANDARD_NO_PAD.encode(nonce_bytes),
        ciphertext: STANDARD_NO_PAD.encode(&ciphertext),
    };
    ciphertext.zeroize();
    let bytes = serde_json::to_vec_pretty(&envelope).map_err(|_| generic_vault_error())?;
    if bytes.len() > MAX_VAULT_BYTES {
        return Err(AppError::new(
            "password_vault_too_large",
            "密码保险库超过安全大小限制。",
        ));
    }
    Ok(bytes)
}

fn validate_payload(payload: &VaultPayload) -> AppResult<()> {
    if payload.schema_version != VAULT_SCHEMA_VERSION || payload.entries.len() > MAX_VAULT_ENTRIES {
        return Err(generic_vault_error());
    }
    let mut ids = HashSet::with_capacity(payload.entries.len());
    let mut accounts = HashSet::with_capacity(payload.entries.len());
    for entry in &payload.entries {
        validate_stored_entry(entry)?;
        if !ids.insert(entry.id.as_str())
            || !accounts.insert((entry.origin.as_str(), entry.username.as_str()))
        {
            return Err(generic_vault_error());
        }
    }
    Ok(())
}

fn validate_stored_entry(entry: &StoredPasswordEntry) -> AppResult<()> {
    validate_entry_id(&entry.id).map_err(|_| generic_vault_error())?;
    validate_site_name(&entry.site_name).map_err(|_| generic_vault_error())?;
    validate_username(&entry.username).map_err(|_| generic_vault_error())?;
    validate_password(&entry.password).map_err(|_| generic_vault_error())?;
    validate_notes(&entry.notes).map_err(|_| generic_vault_error())?;
    validate_template_id(entry.template_id.as_deref()).map_err(|_| generic_vault_error())?;
    if let Some(template) = entry.user_template.as_ref() {
        validate_template_definition(template, &entry.origin, entry.allow_insecure_http)
            .map_err(|_| generic_vault_error())?;
        if entry.template_id.as_deref() != Some(template.id.as_str()) {
            return Err(generic_vault_error());
        }
    }
    let login = validate_login_url(&entry.login_url, entry.allow_insecure_http)
        .map_err(|_| generic_vault_error())?;
    if login.origin != entry.origin
        || login.login_url != entry.login_url
        || login.allow_insecure_http != entry.allow_insecure_http
        || !is_valid_vault_timestamp(&entry.created_at)
        || !is_valid_vault_timestamp(&entry.updated_at)
    {
        return Err(generic_vault_error());
    }
    Ok(())
}

fn entry_summary(entry: &StoredPasswordEntry) -> PasswordEntrySummary {
    PasswordEntrySummary {
        id: entry.id.clone(),
        site_name: entry.site_name.clone(),
        login_url: entry.login_url.clone(),
        origin: entry.origin.clone(),
        username: entry.username.clone(),
        notes: entry.notes.clone(),
        template_id: entry.template_id.clone(),
        allow_insecure_http: entry.allow_insecure_http,
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
    }
}

fn ensure_unique_account(
    entries: &[StoredPasswordEntry],
    origin: &str,
    username: &str,
    except_id: Option<&str>,
) -> AppResult<()> {
    if entries.iter().any(|entry| {
        entry.origin == origin
            && entry.username == username
            && except_id.is_none_or(|id| entry.id != id)
    }) {
        return Err(AppError::new(
            "password_account_exists",
            "这个站点和用户名已存在，请更新现有账户。",
        ));
    }
    Ok(())
}

fn validate_entry_id(value: &str) -> AppResult<()> {
    if value.len() > 128 || Uuid::parse_str(value).is_err() {
        return Err(AppError::invalid("站点账户 ID 无效。"));
    }
    Ok(())
}

fn validate_site_name(value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_SITE_NAME_BYTES {
        return Err(AppError::invalid("站点名称不能为空或过长。"));
    }
    Ok(trimmed.to_string())
}

fn validate_username(value: &str) -> AppResult<()> {
    if value.trim().is_empty() || value.len() > MAX_USERNAME_BYTES || value.contains('\0') {
        return Err(AppError::invalid("用户名不能为空或过长。"));
    }
    Ok(())
}

fn validate_password(value: &str) -> AppResult<()> {
    if value.is_empty() || value.len() > MAX_PASSWORD_BYTES || value.contains('\0') {
        return Err(AppError::invalid("密码不能为空或过长。"));
    }
    Ok(())
}

fn validate_notes(value: &str) -> AppResult<String> {
    if value.len() > MAX_NOTES_BYTES || value.contains('\0') {
        return Err(AppError::invalid("备注内容过长或包含无效字符。"));
    }
    Ok(value.to_string())
}

fn validate_template_id(value: Option<&str>) -> AppResult<Option<String>> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if value.is_some_and(|value| value.len() > MAX_TEMPLATE_ID_BYTES || value.contains('\0')) {
        return Err(AppError::invalid("登录模板 ID 无效或过长。"));
    }
    Ok(value.map(str::to_string))
}

fn validate_template_definition(
    template: &PasswordTemplateDefinition,
    expected_origin: &str,
    allow_insecure_http: bool,
) -> AppResult<()> {
    let id = validate_template_id(Some(&template.id))?
        .ok_or_else(|| invalid_template_error("模板 ID 不能为空。"))?;
    if id != template.id
        || !template.id.starts_with("user-recorded:")
        || template.label.trim().is_empty()
        || template.label != template.label.trim()
        || template.label.len() > MAX_TEMPLATE_LABEL_BYTES
        || template.version == 0
        || template.version > 1_000
        || template.username_selectors.is_empty()
        || template.password_selectors.is_empty()
        || template.username_selectors.len() > MAX_TEMPLATE_SELECTORS
        || template.password_selectors.len() > MAX_TEMPLATE_SELECTORS
    {
        return Err(invalid_template_error("录制的站点模板格式无效。"));
    }
    let (origin, _) = validate_exact_origin(&template.origin, allow_insecure_http)?;
    if origin != expected_origin || origin != template.origin {
        return Err(invalid_template_error(
            "录制模板与账户的精确站点来源不一致。",
        ));
    }
    for selector in template
        .username_selectors
        .iter()
        .chain(template.password_selectors.iter())
    {
        validate_recorded_selector(selector)?;
    }
    Ok(())
}

fn invalid_template_error(message: impl Into<String>) -> AppError {
    AppError::new("password_template_invalid", message)
}

fn validate_recorded_selector(selector: &str) -> AppResult<()> {
    if selector.is_empty()
        || selector.len() > MAX_TEMPLATE_SELECTOR_BYTES
        || selector.trim() != selector
        || !selector.starts_with("input")
        || selector.bytes().any(|byte| byte.is_ascii_control())
        || selector
            .chars()
            .any(|character| matches!(character, ',' | ';' | '{' | '}' | '>' | '+' | '~' | '`'))
    {
        return Err(invalid_template_error("录制模板包含不安全的字段选择器。"));
    }

    let suffix = &selector["input".len()..];
    if suffix.is_empty() {
        return Err(invalid_template_error("录制模板的字段选择器过于宽泛。"));
    }
    if let Some(identifier) = suffix.strip_prefix('#') {
        if identifier.is_empty()
            || !identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid_template_error("录制模板包含无效的字段 ID。"));
        }
        return Ok(());
    }

    let Some(attribute) = suffix
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(invalid_template_error(
            "录制模板只允许 input 字段属性选择器。",
        ));
    };
    let Some((name, quoted_value)) = attribute.split_once('=') else {
        return Err(invalid_template_error("录制模板字段属性必须包含精确值。"));
    };
    if !matches!(
        name,
        "id" | "name" | "type" | "autocomplete" | "aria-label" | "data-testid"
    ) || quoted_value.len() < 2
        || !quoted_value.starts_with('"')
        || !quoted_value.ends_with('"')
    {
        return Err(invalid_template_error("录制模板包含不受支持的字段属性。"));
    }
    let value = &quoted_value[1..quoted_value.len() - 1];
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|character| matches!(character, '"' | '\\' | '[' | ']'))
    {
        return Err(invalid_template_error("录制模板字段属性值无效。"));
    }
    Ok(())
}

/// Parses a login URL and returns the browser-compatible exact origin. Default
/// ports are canonicalized exactly as `location.origin` does.
fn validate_login_url(value: &str, allow_insecure_http: bool) -> AppResult<ValidatedLoginUrl> {
    if value.is_empty() || value.len() > MAX_LOGIN_URL_BYTES {
        return Err(AppError::invalid("登录网址不能为空或过长。"));
    }
    let mut url = Url::parse(value).map_err(|_| AppError::invalid("登录网址格式无效。"))?;
    validate_web_url_security(&url, allow_insecure_http)?;
    url.set_username("")
        .map_err(|_| AppError::invalid("登录网址不能包含用户名或密码。"))?;
    url.set_password(None)
        .map_err(|_| AppError::invalid("登录网址不能包含用户名或密码。"))?;
    let origin = url.origin().ascii_serialization();
    if origin == "null" || origin.len() > MAX_ORIGIN_BYTES {
        return Err(AppError::invalid("登录网址 origin 无效。"));
    }
    Ok(ValidatedLoginUrl {
        login_url: url.to_string(),
        origin,
        allow_insecure_http: url.scheme() == "http",
    })
}

/// Validates an origin received from a browser. Paths, queries, fragments and
/// embedded credentials are rejected rather than silently discarded.
pub fn validate_exact_origin(value: &str, allow_insecure_http: bool) -> AppResult<(String, bool)> {
    if value.is_empty() || value.len() > MAX_ORIGIN_BYTES {
        return Err(AppError::invalid("站点 origin 不能为空或过长。"));
    }
    let url = Url::parse(value).map_err(|_| AppError::invalid("站点 origin 格式无效。"))?;
    validate_web_url_security(&url, allow_insecure_http)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(AppError::invalid(
            "站点 origin 只能包含 scheme、host 和 port。",
        ));
    }
    let origin = url.origin().ascii_serialization();
    if origin == "null" || origin.len() > MAX_ORIGIN_BYTES {
        return Err(AppError::invalid("站点 origin 无效。"));
    }
    Ok((origin, url.scheme() == "http"))
}

fn validate_web_url_security(url: &Url, allow_insecure_http: bool) -> AppResult<()> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::invalid("登录网址不能包含用户名或密码。"));
    }
    if url.host_str().is_none() {
        return Err(AppError::invalid("登录网址必须包含有效主机名。"));
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_insecure_http => Ok(()),
        "http" => Err(AppError::new(
            "password_http_opt_in_required",
            "HTTP 会明文传输登录信息，必须为这个 origin 显式允许后才能保存或填充。",
        )),
        _ => Err(AppError::invalid(
            "密码管理器只支持 HTTPS 和显式允许的 HTTP 网址。",
        )),
    }
}

fn is_valid_vault_timestamp(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TIMESTAMP_BYTES
        && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn vault_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn bytes_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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

fn validate_recovery_password(password: &str) -> AppResult<()> {
    let chars = password.chars().count();
    if chars < RECOVERY_PASSWORD_MIN_CHARS || password.len() > RECOVERY_PASSWORD_MAX_BYTES {
        return Err(AppError::new(
            "password_recovery_password_policy",
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

#[cfg(windows)]
fn protect_local_key(key: &[u8]) -> AppResult<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: key.len().try_into().map_err(|_| generic_vault_error())?,
        pbData: key.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: DPAPI_ENTROPY
            .len()
            .try_into()
            .map_err(|_| generic_vault_error())?,
        pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
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

#[cfg(windows)]
fn unprotect_local_key(wrapped: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: wrapped
            .len()
            .try_into()
            .map_err(|_| generic_vault_error())?,
        pbData: wrapped.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: DPAPI_ENTROPY
            .len()
            .try_into()
            .map_err(|_| generic_vault_error())?,
        pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
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

// Unit tests on non-Windows hosts use a test-only AEAD wrapper. Production on
// non-Windows remains explicitly unsupported; no weak local key fallback is
// ever compiled into a release build.
#[cfg(all(test, not(windows)))]
fn protect_local_key(key: &[u8]) -> AppResult<Vec<u8>> {
    const TEST_KEY: [u8; 32] = [0x5a; 32];
    let mut nonce_bytes = [0u8; 24];
    getrandom::fill(&mut nonce_bytes).map_err(|_| generic_vault_error())?;
    let nonce: XNonce = nonce_bytes.into();
    let cipher = XChaCha20Poly1305::new_from_slice(&TEST_KEY).map_err(|_| generic_vault_error())?;
    let ciphertext = cipher
        .encrypt(&nonce, key)
        .map_err(|_| generic_vault_error())?;
    let mut wrapped = nonce_bytes.to_vec();
    wrapped.extend_from_slice(&ciphertext);
    Ok(wrapped)
}

#[cfg(all(test, not(windows)))]
fn unprotect_local_key(wrapped: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
    const TEST_KEY: [u8; 32] = [0x5a; 32];
    if wrapped.len() <= 24 {
        return Err(generic_vault_error());
    }
    let nonce_array: [u8; 24] = wrapped[..24]
        .try_into()
        .map_err(|_| generic_vault_error())?;
    let nonce: XNonce = nonce_array.into();
    let cipher = XChaCha20Poly1305::new_from_slice(&TEST_KEY).map_err(|_| generic_vault_error())?;
    cipher
        .decrypt(&nonce, &wrapped[24..])
        .map(Zeroizing::new)
        .map_err(|_| generic_vault_error())
}

#[cfg(all(not(test), not(windows)))]
fn protect_local_key(_key: &[u8]) -> AppResult<Vec<u8>> {
    Err(AppError::new(
        "unsupported_platform",
        "密码保险库的本机密钥保护首版仅支持 Windows。",
    ))
}

#[cfg(all(not(test), not(windows)))]
fn unprotect_local_key(_wrapped: &[u8]) -> AppResult<Zeroizing<Vec<u8>>> {
    Err(AppError::new(
        "unsupported_platform",
        "密码保险库的本机密钥保护首版仅支持 Windows。",
    ))
}

fn random_index(length: usize) -> AppResult<usize> {
    if length == 0 {
        return Err(AppError::invalid("密码字符集不能为空。"));
    }
    let upper = (u8::MAX as usize + 1) - ((u8::MAX as usize + 1) % length);
    loop {
        let mut byte = [0u8; 1];
        getrandom::fill(&mut byte).map_err(|_| generic_vault_error())?;
        let value = byte[0] as usize;
        if value < upper {
            return Ok(value % length);
        }
    }
}

pub fn generate_password_value(options: PasswordGeneratorOptions) -> AppResult<SensitiveValue> {
    if !(8..=128).contains(&options.length) {
        return Err(AppError::invalid(
            "生成密码长度必须在 8 到 128 个字符之间。",
        ));
    }
    let mut classes = Vec::<&[u8]>::new();
    if options.uppercase {
        classes.push(if options.exclude_ambiguous {
            b"ABCDEFGHJKLMNPQRSTUVWXYZ"
        } else {
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        });
    }
    if options.lowercase {
        classes.push(if options.exclude_ambiguous {
            b"abcdefghijkmnopqrstuvwxyz"
        } else {
            b"abcdefghijklmnopqrstuvwxyz"
        });
    }
    if options.digits {
        classes.push(if options.exclude_ambiguous {
            b"23456789"
        } else {
            b"0123456789"
        });
    }
    if options.symbols {
        classes.push(b"!@#$%^&*()-_=+[]{}:,.?/");
    }
    if classes.is_empty() {
        return Err(AppError::invalid("至少选择一种密码字符类型。"));
    }
    let mut alphabet = Vec::new();
    for class in &classes {
        alphabet.extend_from_slice(class);
    }
    let mut output = Zeroizing::new(Vec::with_capacity(options.length));
    // Guarantee every selected class once, then fill the remaining positions.
    for class in &classes {
        output.push(class[random_index(class.len())?]);
    }
    while output.len() < options.length {
        output.push(alphabet[random_index(alphabet.len())?]);
    }
    // Fisher-Yates with rejection-sampled indices keeps the class guarantee
    // while avoiding modulo bias.
    for index in (1..output.len()).rev() {
        let swap = random_index(index + 1)?;
        output.swap(index, swap);
    }
    let password = String::from_utf8(output.to_vec()).map_err(|_| generic_vault_error())?;
    Ok(SensitiveValue(password))
}

#[cfg(windows)]
fn clipboard_marker(value: &str) -> AppResult<Vec<u8>> {
    let mut salt = [0u8; 32];
    getrandom::fill(&mut salt).map_err(|_| generic_vault_error())?;
    let units = value.encode_utf16().collect::<Vec<_>>();
    let mut hasher = Sha256::new();
    hasher.update(salt);
    for unit in units {
        hasher.update(unit.to_le_bytes());
    }
    let mut marker = salt.to_vec();
    marker.extend_from_slice(&hasher.finalize());
    Ok(marker)
}

#[cfg(windows)]
fn clipboard_marker_for_units(units: &[u16], marker: &[u8]) -> bool {
    if marker.len() != 64 {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(&marker[..32]);
    for unit in units {
        hasher.update(unit.to_le_bytes());
    }
    constant_time_eq(&marker[32..], &hasher.finalize())
}

fn write_sensitive_clipboard(
    value: &str,
    lease: &Arc<Mutex<Option<ClipboardLease>>>,
) -> AppResult<()> {
    #[cfg(windows)]
    {
        use std::ptr::null_mut;
        use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        };
        use windows_sys::Win32::System::Memory::{
            GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
        };

        fn allocate_global(bytes: &[u8]) -> AppResult<HGLOBAL> {
            let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
            if handle.is_null() {
                return Err(AppError::new("clipboard_error", "分配剪贴板内存失败。"));
            }
            let destination = unsafe { GlobalLock(handle) } as *mut u8;
            if destination.is_null() {
                unsafe {
                    let _ = GlobalFree(handle);
                }
                return Err(AppError::new("clipboard_error", "锁定剪贴板内存失败。"));
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
                let _ = GlobalUnlock(handle);
            }
            Ok(handle)
        }

        fn register_clipboard_format(name: &str) -> AppResult<u32> {
            let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
            if format == 0 {
                Err(AppError::new(
                    "clipboard_error",
                    "注册密码剪贴板隐私格式失败。",
                ))
            } else {
                Ok(format)
            }
        }

        let mut units = Zeroizing::new(value.encode_utf16().collect::<Vec<_>>());
        units.push(0);
        let bytes_len = units
            .len()
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or_else(|| AppError::invalid("剪贴板文本过长。"))?;
        let marker = clipboard_marker(value)?;
        let exclude_format =
            register_clipboard_format("ExcludeClipboardContentFromMonitorProcessing")?;
        let history_format = register_clipboard_format("CanIncludeInClipboardHistory")?;
        let cloud_format = register_clipboard_format("CanUploadToCloudClipboard")?;
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
                "系统剪贴板暂时被其他程序占用。",
            ));
        }
        if unsafe { EmptyClipboard() } == 0 {
            unsafe { CloseClipboard() };
            return Err(AppError::new("clipboard_error", "清空剪贴板失败。"));
        }
        const CF_UNICODETEXT: u32 = 13;
        let text_bytes =
            unsafe { std::slice::from_raw_parts(units.as_ptr() as *const u8, bytes_len) };
        let zero = 0u32.to_ne_bytes();
        for (format, bytes) in [
            (CF_UNICODETEXT, text_bytes),
            (exclude_format, zero.as_slice()),
            (history_format, zero.as_slice()),
            (cloud_format, zero.as_slice()),
        ] {
            let handle = match allocate_global(bytes) {
                Ok(handle) => handle,
                Err(error) => {
                    unsafe {
                        let _ = EmptyClipboard();
                        CloseClipboard();
                    }
                    return Err(error);
                }
            };
            if unsafe { SetClipboardData(format, handle) }.is_null() {
                unsafe {
                    let _ = GlobalFree(handle);
                    let _ = EmptyClipboard();
                    CloseClipboard();
                }
                return Err(AppError::new("clipboard_error", "写入剪贴板失败。"));
            }
        }
        let sequence = unsafe { GetClipboardSequenceNumber() };
        unsafe { CloseClipboard() };
        *lock_unpoisoned(lease) = Some(ClipboardLease {
            sequence,
            marker,
            clear_at: Instant::now() + CLIPBOARD_TTL,
        });
        schedule_clipboard_cleanup(Arc::downgrade(lease), Instant::now() + CLIPBOARD_TTL);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (value, lease);
        Err(AppError::new(
            "unsupported_platform",
            "密码剪贴板仅支持 Windows。",
        ))
    }
}

fn force_expire_clipboard(lease: &Arc<Mutex<Option<ClipboardLease>>>) {
    if let Some(value) = lock_unpoisoned(lease).as_mut() {
        value.clear_at = Instant::now();
    }
}

fn schedule_clipboard_cleanup(lease: Weak<Mutex<Option<ClipboardLease>>>, clear_at: Instant) {
    std::thread::spawn(move || {
        std::thread::sleep(clear_at.saturating_duration_since(Instant::now()));
        let retry_until = Instant::now() + Duration::from_secs(35);
        loop {
            let Some(lease) = lease.upgrade() else {
                return;
            };
            if clear_clipboard_now(&lease) || Instant::now() >= retry_until {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

fn clear_clipboard_now(lease: &Arc<Mutex<Option<ClipboardLease>>>) -> bool {
    #[cfg(windows)]
    {
        use std::ptr::null_mut;
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
            IsClipboardFormatAvailable, OpenClipboard,
        };
        use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
        const CF_UNICODETEXT: u32 = 13;
        let mut state = lock_unpoisoned(lease);
        let Some(existing) = state.as_ref() else {
            return true;
        };
        if existing.clear_at > Instant::now() {
            return false;
        }
        if unsafe { GetClipboardSequenceNumber() } != existing.sequence {
            state.take();
            return true;
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
            return false;
        }
        let mut matches = false;
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } != 0 {
            let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
            if !handle.is_null() {
                let size = unsafe { GlobalSize(handle) } as usize;
                if size >= 2 && size <= MAX_PASSWORD_BYTES.saturating_mul(4) {
                    let ptr = unsafe { GlobalLock(handle) } as *const u16;
                    if !ptr.is_null() {
                        let count = size / 2;
                        let units = unsafe { std::slice::from_raw_parts(ptr, count) };
                        let end = units.iter().position(|value| *value == 0).unwrap_or(count);
                        matches = clipboard_marker_for_units(&units[..end], &existing.marker);
                        unsafe {
                            let _ = GlobalUnlock(handle);
                        }
                    }
                }
            }
        }
        if matches {
            let _ = unsafe { EmptyClipboard() };
        }
        unsafe { CloseClipboard() };
        state.take();
        matches
    }
    #[cfg(not(windows))]
    {
        let _ = lease;
        true
    }
}

fn ensure_password_window(window: &WebviewWindow) -> AppResult<()> {
    match window.label() {
        "passwords" | "password-manager" => Ok(()),
        _ => Err(AppError::new(
            "password_window_required",
            "此操作只能在密码管理器窗口中执行。",
        )),
    }
}

#[tauri::command]
pub async fn get_password_status(
    app: AppHandle,
    window: WebviewWindow,
) -> AppResult<PasswordStatus> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    let task_app = app.clone();
    let status = tauri::async_runtime::spawn_blocking(move || -> AppResult<PasswordStatus> {
        let status = task_app.state::<PasswordStore>().status_at(epoch)?;
        crate::recovery::annotate_password_status(&task_app.state::<crate::mfa::MfaStore>(), status)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "读取密码状态任务异常结束。"))??;
    let sync_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::password_browser::sync_capture_from_store(&sync_app);
    });
    Ok(status)
}

#[tauri::command]
pub async fn list_password_entries(
    app: AppHandle,
    window: WebviewWindow,
) -> AppResult<Vec<PasswordEntrySummary>> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>().list_entries_at(epoch)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "读取密码账户任务异常结束。"))?
}

#[tauri::command]
pub async fn create_password_entry(
    app: AppHandle,
    window: WebviewWindow,
    input: PasswordEntryInput,
) -> AppResult<PasswordEntrySummary> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>().create_entry_at(input, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "保存密码账户任务异常结束。"))?
}

#[tauri::command]
pub async fn update_password_entry(
    app: AppHandle,
    window: WebviewWindow,
    input: PasswordEntryUpdateInput,
) -> AppResult<PasswordEntrySummary> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>().update_entry_at(input, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "更新密码账户任务异常结束。"))?
}

#[tauri::command]
pub async fn delete_password_entry(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<()> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>()
            .delete_entry_at(&entry_id, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "删除密码账户任务异常结束。"))?
}

#[tauri::command]
pub async fn reveal_password(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<PasswordRevealResult> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>()
            .reveal_password_at(&entry_id, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "显示密码任务异常结束。"))?
}

#[tauri::command]
pub async fn copy_password_username(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<()> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>()
            .copy_field_at(&entry_id, false, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_copy_error", "复制用户名任务异常结束。"))?
}

#[tauri::command]
pub async fn copy_password_secret(
    app: AppHandle,
    window: WebviewWindow,
    entry_id: String,
) -> AppResult<()> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>()
            .copy_field_at(&entry_id, true, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_copy_error", "复制密码任务异常结束。"))?
}

#[tauri::command]
pub async fn evaluate_password_capture(
    app: AppHandle,
    window: WebviewWindow,
    candidate: PasswordCaptureCandidate,
) -> AppResult<PasswordCaptureDecision> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<PasswordStore>()
            .capture_decision_at(candidate, epoch)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "检测登录信息任务异常结束。"))?
}

#[tauri::command]
pub async fn set_password_capture_enabled(
    app: AppHandle,
    window: WebviewWindow,
    enabled: bool,
) -> AppResult<PasswordStatus> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    let task_app = app.clone();
    let status = tauri::async_runtime::spawn_blocking(move || -> AppResult<PasswordStatus> {
        let baseline = task_app.state::<PasswordStore>().status_at(epoch)?;
        let shared_recovery_configured = crate::recovery::annotate_password_status(
            &task_app.state::<crate::mfa::MfaStore>(),
            baseline,
        )?
        .shared_recovery_configured;
        let mut status = task_app
            .state::<PasswordStore>()
            .set_capture_enabled_at(enabled, epoch)?;
        status.shared_recovery_configured |= shared_recovery_configured;
        Ok(status)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "更新登录检测设置任务异常结束。"))??;
    let sync_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::password_browser::sync_capture_from_store(&sync_app);
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "同步 Firefox 登录检测任务异常结束。"))?;
    Ok(status)
}

#[tauri::command]
pub async fn configure_password_recovery_password(
    app: AppHandle,
    window: WebviewWindow,
    password: SensitiveText,
    current_password: Option<SensitiveText>,
) -> AppResult<PasswordStatus> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    let task_app = app.clone();
    let status = tauri::async_runtime::spawn_blocking(move || {
        crate::recovery::configure_shared_recovery_password(
            &task_app.state::<crate::mfa::MfaStore>(),
            &task_app.state::<PasswordStore>(),
            password.as_str(),
            current_password.as_ref().map(SensitiveText::as_str),
        )?;
        let status = task_app.state::<PasswordStore>().status_at(epoch)?;
        crate::recovery::annotate_password_status(&task_app.state::<crate::mfa::MfaStore>(), status)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "设置恢复密码任务异常结束。"))??;
    let sync_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::password_browser::sync_capture_from_store(&sync_app);
    });
    Ok(status)
}

#[tauri::command]
pub async fn unlock_passwords_with_recovery_password(
    app: AppHandle,
    window: WebviewWindow,
    password: SensitiveText,
) -> AppResult<PasswordStatus> {
    ensure_password_window(&window)?;
    let epoch = app.state::<PasswordStore>().require_active_epoch()?;
    let task_app = app.clone();
    let status = tauri::async_runtime::spawn_blocking(move || {
        let status = task_app
            .state::<PasswordStore>()
            .unlock_with_recovery_password_at(password.as_str(), epoch)?;
        crate::recovery::annotate_password_status(&task_app.state::<crate::mfa::MfaStore>(), status)
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "恢复密码保险库任务异常结束。"))??;
    let sync_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::password_browser::sync_capture_from_store(&sync_app);
    });
    Ok(status)
}

#[tauri::command]
pub fn generate_password(
    options: Option<PasswordGeneratorOptions>,
) -> AppResult<GeneratedPassword> {
    Ok(GeneratedPassword {
        password: generate_password_value(options.unwrap_or_default())?,
    })
}

#[tauri::command]
pub async fn lock_password_vault(app: AppHandle, window: WebviewWindow) -> AppResult<()> {
    ensure_password_window(&window)?;
    let task_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        task_app.state::<PasswordStore>().lock_current_session()
    })
    .await
    .map_err(|_| AppError::new("password_task_error", "锁定密码保险库任务异常结束。"))??;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<crate::password_browser::PasswordBrowserService>()
            .suspend_capture();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    const RECOVERY_PASSWORD: &str = "petaldesk-password-recovery";

    fn input(
        site_name: &str,
        login_url: &str,
        username: &str,
        password: &str,
    ) -> PasswordEntryInput {
        PasswordEntryInput {
            site_name: site_name.to_string(),
            login_url: login_url.to_string(),
            username: SensitiveText(username.to_string()),
            password: SensitiveText(password.to_string()),
            notes: "test note".to_string(),
            template_id: Some("generic".to_string()),
            allow_insecure_http: false,
        }
    }

    fn candidate(origin: &str, username: &str, password: &str) -> PasswordCaptureCandidate {
        PasswordCaptureCandidate {
            origin: origin.to_string(),
            username: SensitiveText(username.to_string()),
            password: SensitiveText(password.to_string()),
            allow_insecure_http: false,
        }
    }

    fn test_store() -> (TempDir, PasswordStore) {
        let root = tempdir().unwrap();
        let store = PasswordStore::load(root.path()).unwrap();
        store.activate();
        store
            .configure_recovery_password(RECOVERY_PASSWORD, None)
            .unwrap();
        (root, store)
    }

    #[test]
    fn encrypted_vault_round_trips_and_disk_has_no_plaintext() {
        let (root, store) = test_store();
        let saved = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice@example.com",
                "correct horse battery staple",
            ))
            .unwrap();
        let password_path = root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join(PASSWORDS_DIR);
        let mut files = vec![password_path.join(VAULT_FILE)];
        files.extend(
            std::fs::read_dir(password_path.join(BACKUP_DIR))
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path()),
        );
        for file in files {
            let bytes = std::fs::read(file).unwrap();
            assert!(!bytes
                .windows("correct horse battery staple".len())
                .any(|part| part == b"correct horse battery staple"));
            assert!(!bytes
                .windows("alice@example.com".len())
                .any(|part| part == b"alice@example.com"));
        }
        drop(store);

        let reopened = PasswordStore::load(root.path()).unwrap();
        reopened.activate();
        let entries = reopened.list_entries().unwrap();
        assert_eq!(entries, vec![saved.clone()]);
        let revealed = reopened.reveal_password(&saved.id).unwrap();
        assert_eq!(revealed.password.as_str(), "correct horse battery staple");
        reopened.delete_entry(&saved.id).unwrap();
        assert!(reopened.list_entries().unwrap().is_empty());
        reopened.lock();
        assert_eq!(
            reopened.list_entries().unwrap_err().code,
            "password_session_closed"
        );
    }

    #[test]
    fn capture_matching_uses_origin_and_username() {
        let (_root, store) = test_store();
        store.set_capture_enabled(true).unwrap();
        let saved = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "one",
            ))
            .unwrap();
        let same = store
            .capture_decision(candidate("https://example.com/", "alice", "one"))
            .unwrap();
        assert_eq!(same.action, PasswordCaptureAction::NoPrompt);
        assert_eq!(same.entry_id.as_deref(), Some(saved.id.as_str()));
        let update = store
            .capture_decision(candidate("https://example.com", "alice", "two"))
            .unwrap();
        assert_eq!(update.action, PasswordCaptureAction::Update);
        assert_eq!(update.entry_id.as_deref(), Some(saved.id.as_str()));
        let create = store
            .capture_decision(candidate("https://example.com", "bob", "one"))
            .unwrap();
        assert_eq!(create.action, PasswordCaptureAction::Create);
        assert!(create.entry_id.is_none());
        let other_origin = store
            .capture_decision(candidate("https://other.example.com", "alice", "one"))
            .unwrap();
        assert_eq!(other_origin.action, PasswordCaptureAction::Create);
        let disabled_store = {
            let root = tempdir().unwrap();
            let store = PasswordStore::load(root.path()).unwrap();
            store.activate();
            store
        };
        let disabled = disabled_store
            .capture_decision(candidate("https://example.com", "alice", "one"))
            .unwrap();
        assert_eq!(disabled.action, PasswordCaptureAction::Disabled);
    }

    #[test]
    fn capture_without_username_returns_same_origin_account_choices() {
        let (_root, store) = test_store();
        store.set_capture_enabled(true).unwrap();
        let first = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "one",
            ))
            .unwrap();
        let single = store
            .capture_decision(candidate("https://example.com", "", "two"))
            .unwrap();
        assert_eq!(single.action, PasswordCaptureAction::Update);
        assert_eq!(single.entry_id.as_deref(), Some(first.id.as_str()));
        assert_eq!(single.account_choices.len(), 1);

        store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "bob",
                "three",
            ))
            .unwrap();
        let multiple = store
            .capture_decision(candidate("https://example.com", "", "four"))
            .unwrap();
        assert_eq!(multiple.action, PasswordCaptureAction::SelectAccount);
        assert_eq!(multiple.entry_id, None);
        assert_eq!(multiple.account_choices.len(), 2);

        let no_match = store
            .capture_decision(candidate("https://other.example.com", "", "five"))
            .unwrap();
        assert_eq!(no_match.action, PasswordCaptureAction::UsernameRequired);
        assert!(no_match.account_choices.is_empty());
    }

    #[test]
    fn origin_validation_is_https_by_default_and_exact() {
        let login = validate_login_url("https://Example.com:443/login", false).unwrap();
        assert_eq!(login.origin, "https://example.com");
        assert_eq!(login.login_url, "https://example.com/login");
        assert!(validate_login_url("http://example.com/login", false).is_err());
        let http = validate_login_url("http://example.com/login", true).unwrap();
        assert_eq!(http.origin, "http://example.com");
        assert!(validate_exact_origin("https://example.com/login", false).is_err());
        assert!(validate_exact_origin("https://user:pass@example.com/", false).is_err());
        assert!(validate_exact_origin("ftp://example.com/", false).is_err());
        let (origin, insecure) = validate_exact_origin("https://example.com/", false).unwrap();
        assert_eq!(origin, "https://example.com");
        assert!(!insecure);
        assert!(validate_exact_origin("http://example.com/", false).is_err());
        let (origin, insecure) = validate_exact_origin("http://example.com/", true).unwrap();
        assert_eq!(origin, "http://example.com");
        assert!(insecure);
    }

    #[test]
    fn recorded_template_is_origin_bound_encrypted_state_and_returns_as_a_full_object() {
        let (_root, store) = test_store();
        let entry = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "secret",
            ))
            .unwrap();
        let epoch = store.require_active_epoch().unwrap();
        let saved =
            store
                .set_recorded_template_at(
                    &entry.id,
                    PasswordTemplateDefinition {
                        id: "extension-provided-id".to_string(),
                        label: "Example 用户模板".to_string(),
                        version: 1,
                        mode: PasswordTemplateMode::Password,
                        origin: "https://example.com".to_string(),
                        username_selectors: vec!["input[name=\"email\"]".to_string()],
                        password_selectors: vec![
                            "input[autocomplete=\"current-password\"]".to_string()
                        ],
                    },
                    epoch,
                )
                .unwrap();
        assert_eq!(
            saved.template_id.as_deref(),
            Some(format!("user-recorded:{}", entry.id).as_str())
        );
        let fill = store.browser_fill_data(&entry.id).unwrap();
        let template = fill.user_template.clone().unwrap();
        assert_eq!(template.id, format!("user-recorded:{}", entry.id));
        assert_eq!(template.origin, "https://example.com");
        assert_eq!(template.username_selectors, ["input[name=\"email\"]"]);
    }

    #[test]
    fn recorded_template_rejects_wrong_origin_and_unconstrained_selectors() {
        let (_root, store) = test_store();
        let entry = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "secret",
            ))
            .unwrap();
        let epoch = store.require_active_epoch().unwrap();
        for (origin, username_selector) in [
            ("https://other.example.com", "input[name=\"email\"]"),
            ("https://example.com", "input, body"),
            ("https://example.com", "input[type=\"text\"] > span"),
        ] {
            let error = store
                .set_recorded_template_at(
                    &entry.id,
                    PasswordTemplateDefinition {
                        id: "extension-id".to_string(),
                        label: "Example 用户模板".to_string(),
                        version: 1,
                        mode: PasswordTemplateMode::Password,
                        origin: origin.to_string(),
                        username_selectors: vec![username_selector.to_string()],
                        password_selectors: vec!["input[type=\"password\"]".to_string()],
                    },
                    epoch,
                )
                .unwrap_err();
            assert_eq!(error.code, "password_template_invalid");
        }
        assert!(store
            .browser_fill_data(&entry.id)
            .unwrap()
            .user_template
            .is_none());
    }

    #[test]
    fn damaged_primary_recovers_from_authenticated_backup() {
        let (root, store) = test_store();
        let saved = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "one",
            ))
            .unwrap();
        let mut update = PasswordEntryUpdateInput {
            id: saved.id.clone(),
            site_name: "Example updated".to_string(),
            login_url: saved.login_url.clone(),
            username: SensitiveText("alice".to_string()),
            password: Some(SensitiveText("two".to_string())),
            notes: String::new(),
            template_id: None,
            allow_insecure_http: false,
        };
        store.update_entry(update).unwrap();
        let vault_path = root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join(PASSWORDS_DIR)
            .join(VAULT_FILE);
        atomic_write(&vault_path, b"broken primary").unwrap();
        drop(store);
        let recovered = PasswordStore::load(root.path()).unwrap();
        recovered.activate();
        let status = recovered.status().unwrap();
        assert!(status.recovered_from_backup);
        let entries = recovered.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, saved.id);
        update = PasswordEntryUpdateInput {
            id: entries[0].id.clone(),
            site_name: "Example final".to_string(),
            login_url: entries[0].login_url.clone(),
            username: SensitiveText("alice".to_string()),
            password: None,
            notes: String::new(),
            template_id: None,
            allow_insecure_http: false,
        };
        recovered.update_entry(update).unwrap();
    }

    #[test]
    fn recovery_password_rebinds_a_vault_without_a_local_key() {
        let (root, store) = test_store();
        let saved = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "portable secret",
            ))
            .unwrap();
        let vault_path = root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join(PASSWORDS_DIR)
            .join(VAULT_FILE);
        drop(store);

        let bytes = std::fs::read(&vault_path).unwrap();
        let mut envelope: VaultEnvelope = serde_json::from_slice(&bytes).unwrap();
        envelope.dpapi_wrapped_key = STANDARD_NO_PAD.encode([0x5a; 64]);
        atomic_write(&vault_path, &serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();

        let copied = PasswordStore::load(root.path()).unwrap();
        copied.activate();
        let status = copied.status().unwrap();
        assert!(!status.available);
        assert_eq!(
            status.recovery_state,
            PasswordRecoveryState::PasswordRequired
        );
        assert_eq!(
            copied
                .unlock_with_recovery_password("definitely-wrong-password")
                .unwrap_err()
                .code,
            "password_recovery_password_invalid"
        );
        let status = copied
            .unlock_with_recovery_password(RECOVERY_PASSWORD)
            .unwrap();
        assert!(status.available);
        assert_eq!(status.recovery_state, PasswordRecoveryState::Ready);
        assert_eq!(copied.list_entries().unwrap()[0].id, saved.id);
    }

    #[test]
    fn explicit_lock_keeps_window_session_available_for_recovery_unlock() {
        let (_root, store) = test_store();
        let saved = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "locked secret",
            ))
            .unwrap();
        let epoch_before = store.session.epoch.load(Ordering::Acquire);

        store.lock_current_session().unwrap();

        assert!(store.session.active.load(Ordering::Acquire));
        assert!(store.session.epoch.load(Ordering::Acquire) > epoch_before);
        assert!(lock_unpoisoned(&store.runtime).vault.is_none());
        let status = store.status().unwrap();
        assert!(status.available);
        assert!(status.locked);
        assert_eq!(status.entry_count, 1);
        assert_eq!(
            store.list_entries().unwrap_err().code,
            "password_vault_locked"
        );

        let status = store
            .unlock_with_recovery_password(RECOVERY_PASSWORD)
            .unwrap();
        assert!(status.available);
        assert!(!status.locked);
        assert_eq!(
            store.reveal_password(&saved.id).unwrap().password.as_str(),
            "locked secret"
        );
    }

    #[test]
    fn window_close_expires_the_session_and_reopen_starts_a_new_epoch() {
        let (_root, store) = test_store();
        let entry = store
            .create_entry(input(
                "Example",
                "https://example.com/login",
                "alice",
                "stored secret",
            ))
            .unwrap();
        let first = store.activate();
        store.lock();
        assert_eq!(
            store.list_entries().unwrap_err().code,
            "password_session_closed"
        );
        let second = store.activate();
        assert!(second > first);
        let status = store.status().unwrap();
        assert!(!status.locked);
        assert_eq!(status.entry_count, 1);
        assert!(store.session.active.load(Ordering::Acquire));
        assert_eq!(
            store.reveal_password(&entry.id).unwrap().password.as_str(),
            "stored secret"
        );
    }

    #[test]
    fn generated_password_honors_selected_classes_and_length() {
        let value = generate_password_value(PasswordGeneratorOptions {
            length: 32,
            uppercase: true,
            lowercase: true,
            digits: true,
            symbols: true,
            exclude_ambiguous: true,
        })
        .unwrap();
        assert_eq!(value.as_str().chars().count(), 32);
        assert!(value.as_str().chars().any(|ch| ch.is_ascii_uppercase()));
        assert!(value.as_str().chars().any(|ch| ch.is_ascii_lowercase()));
        assert!(value.as_str().chars().any(|ch| ch.is_ascii_digit()));
        assert!(value.as_str().chars().any(|ch| !ch.is_ascii_alphanumeric()));
        assert!(!value.as_str().contains(['0', '1', 'I', 'O', 'l']));
        assert!(generate_password_value(PasswordGeneratorOptions {
            length: 7,
            ..Default::default()
        })
        .is_err());
        assert!(generate_password_value(PasswordGeneratorOptions {
            length: 12,
            uppercase: false,
            lowercase: false,
            digits: false,
            symbols: false,
            exclude_ambiguous: true,
        })
        .is_err());
    }
}
