//! Coordinates the one recovery password shared by the MFA and password vaults.

use crate::error::{AppError, AppResult};
use crate::mfa::MfaStore;
use crate::passwords::{PasswordStatus, PasswordStore};
use crate::storage::{
    atomic_write, atomic_write_json, ensure_managed_subdirectory, INTERNAL_DATA_DIR,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use uuid::Uuid;

const JOURNAL_SCHEMA_VERSION: u32 = 1;
const JOURNAL_FILE: &str = "shared-recovery-journal.json";
const MFA_SNAPSHOT_FILE: &str = "mfa-vault.snapshot";
const PASSWORD_SNAPSHOT_FILE: &str = "password-vault.snapshot";
const MAX_JOURNAL_BYTES: u64 = 64 * 1024;
const MAX_MFA_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PASSWORD_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;

static SHARED_RECOVERY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultSnapshotRecord {
    existed: bool,
    byte_len: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SharedRecoveryJournal {
    schema_version: u32,
    transaction_id: String,
    state: SharedRecoveryJournalState,
    mfa: VaultSnapshotRecord,
    passwords: VaultSnapshotRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SharedRecoveryJournalState {
    Prepared,
    Committed,
}

struct SharedRecoveryTransaction {
    recovery_root: PathBuf,
    stage_path: PathBuf,
    journal_path: PathBuf,
    journal: SharedRecoveryJournal,
}

pub(crate) fn recover_interrupted_shared_recovery(data_storage_path: &Path) -> AppResult<bool> {
    let recovery_root =
        ensure_managed_subdirectory(data_storage_path, &[INTERNAL_DATA_DIR, "tools", "recovery"])?;
    let journal_path = recovery_root.join(JOURNAL_FILE);
    if !journal_path.exists() {
        cleanup_orphan_stages(&recovery_root, None)?;
        return Ok(false);
    }
    let journal = read_journal(&journal_path)?;
    let paths = recovery_paths(data_storage_path, &recovery_root, &journal)?;
    if journal.state == SharedRecoveryJournalState::Prepared {
        restore_from_journal(&journal, &paths)?;
    }
    cleanup_stage(&paths.stage_path)?;
    remove_file_durable(&journal_path, "提交共享恢复密码中断回滚")?;
    cleanup_orphan_stages(&recovery_root, None)?;
    Ok(true)
}

pub(crate) fn configure_shared_recovery_password(
    mfa: &MfaStore,
    passwords: &PasswordStore,
    password: &str,
    current_password: Option<&str>,
) -> AppResult<()> {
    configure_shared_recovery_password_with(
        mfa,
        passwords,
        password,
        current_password,
        |passwords, password, current| {
            passwords.configure_shared_recovery_password_locked(password, current)
        },
    )
}

/// Enriches password-manager status with the global recovery-password state.
///
/// A vault that does not exist yet still reports its own state as
/// `setup-required`; this additional bit lets the UI distinguish linking that
/// vault to the recovery password already used by MFA from creating the first
/// global recovery password.
pub(crate) fn annotate_password_status(
    mfa: &MfaStore,
    mut status: PasswordStatus,
) -> AppResult<PasswordStatus> {
    if status.recovery_state != crate::passwords::PasswordRecoveryState::SetupRequired {
        return Ok(status);
    }
    let _coordinator = lock_unpoisoned(&SHARED_RECOVERY_LOCK);
    let mfa_transaction = mfa.shared_recovery_transaction_lock();
    status.shared_recovery_configured = mfa.shared_recovery_is_configured_locked()?;
    drop(mfa_transaction);
    Ok(status)
}

pub(crate) fn configure_recovery_password_from_mfa(
    mfa: &MfaStore,
    passwords: &PasswordStore,
    password: &str,
    current_password: Option<&str>,
) -> AppResult<()> {
    #[cfg(not(windows))]
    {
        let _ = passwords;
        return mfa.configure_shared_recovery_password(password, current_password);
    }
    #[cfg(windows)]
    configure_shared_recovery_password(mfa, passwords, password, current_password)
}

fn configure_shared_recovery_password_with<F>(
    mfa: &MfaStore,
    passwords: &PasswordStore,
    password: &str,
    current_password: Option<&str>,
    configure_passwords: F,
) -> AppResult<()>
where
    F: FnOnce(&PasswordStore, &str, Option<&str>) -> AppResult<()>,
{
    let _coordinator = lock_unpoisoned(&SHARED_RECOVERY_LOCK);
    // Every regular vault mutation takes its store lifecycle lock. Holding the
    // two locks in this fixed order keeps the snapshot and both writes one
    // indivisible operation with respect to MFA and password UI commands.
    let mfa_transaction = mfa.shared_recovery_transaction_lock();
    let password_transaction = passwords.shared_recovery_transaction_lock();
    let mfa_configured = mfa.shared_recovery_is_configured_locked()?;
    let passwords_configured = passwords.shared_recovery_is_configured_locked()?;

    if mfa_configured && passwords_configured {
        if current_password.is_none() {
            // A status read can race with the other tool finishing first-time
            // setup. Submitting the already-current password is a safe no-op;
            // a different password still cannot rotate either vault.
            mfa.verify_shared_recovery_password_locked(password)?;
            passwords.verify_shared_recovery_password_locked(password)?;
            return Ok(());
        }
        let current = current_password.expect("current password presence was checked");
        mfa.verify_shared_recovery_password_locked(current)?;
        passwords.verify_shared_recovery_password_locked(current)?;
    } else if mfa_configured {
        if let Some(current) = current_password {
            mfa.verify_shared_recovery_password_locked(current)?;
        } else {
            // First-time password-vault setup must reuse the existing MFA
            // recovery password instead of silently creating a second one.
            mfa.verify_shared_recovery_password_locked(password)?;
        }
    } else if passwords_configured {
        if let Some(current) = current_password {
            passwords.verify_shared_recovery_password_locked(current)?;
        } else {
            passwords.verify_shared_recovery_password_locked(password)?;
        }
    }

    let transaction = SharedRecoveryTransaction::prepare(mfa, passwords)?;
    let mut configure_passwords = Some(configure_passwords);
    let mut update_passwords = |current: Option<&str>| {
        configure_passwords
            .take()
            .expect("password recovery update can run only once")(
            passwords, password, current
        )
    };
    let result: AppResult<()> = (|| {
        if mfa_configured && passwords_configured {
            let current = current_password.expect("current password was preflighted");
            mfa.configure_shared_recovery_password_locked(password, Some(current))?;
            update_passwords(Some(current))
        } else if mfa_configured {
            if let Some(current) = current_password {
                mfa.configure_shared_recovery_password_locked(password, Some(current))?;
                update_passwords(None)
            } else {
                update_passwords(None)
            }
        } else if passwords_configured {
            if let Some(current) = current_password {
                mfa.configure_shared_recovery_password_locked(password, None)?;
                update_passwords(Some(current))
            } else {
                mfa.configure_shared_recovery_password_locked(password, None)
            }
        } else {
            mfa.configure_shared_recovery_password_locked(password, None)?;
            update_passwords(None)
        }
    })();

    let (outcome, restored) = match result {
        Ok(()) => match transaction.commit() {
            Ok(()) => (Ok(()), false),
            Err(commit_error) => match transaction.rollback_disk(mfa, passwords) {
                Ok(()) => (Err(commit_error), true),
                Err(rollback) => (Err(rollback_error(commit_error, rollback)), false),
            },
        },
        Err(update) => match transaction.rollback_disk(mfa, passwords) {
            Ok(()) => (Err(update), true),
            Err(rollback) => (Err(rollback_error(update, rollback)), false),
        },
    };
    drop(password_transaction);
    drop(mfa_transaction);
    if restored {
        mfa.refresh_after_shared_recovery_restore();
        passwords.refresh_after_shared_recovery_restore();
    }
    outcome
}

impl SharedRecoveryTransaction {
    fn prepare(mfa: &MfaStore, passwords: &PasswordStore) -> AppResult<Self> {
        let recovery_root = passwords.shared_recovery_transaction_path().to_path_buf();
        validate_runtime_paths(mfa, passwords, &recovery_root)?;
        let journal_path = recovery_root.join(JOURNAL_FILE);
        if journal_path.exists() {
            return Err(AppError::new(
                "shared_recovery_pending_transaction",
                "检测到未恢复的共享恢复密码事务，请重启飞花完成自动恢复。",
            ));
        }

        let mfa_snapshot = mfa.shared_recovery_snapshot_locked()?;
        let password_snapshot = passwords.shared_recovery_snapshot_locked()?;
        let transaction_id = Uuid::new_v4().to_string();
        let stage_name = format!("shared-recovery-{transaction_id}");
        let stage_path = ensure_managed_subdirectory(&recovery_root, &[stage_name.as_str()])?;
        let result = (|| {
            let mfa_record =
                write_snapshot(&stage_path.join(MFA_SNAPSHOT_FILE), mfa_snapshot.as_deref())?;
            let password_record = write_snapshot(
                &stage_path.join(PASSWORD_SNAPSHOT_FILE),
                password_snapshot.as_deref(),
            )?;
            if !snapshot_matches(mfa.shared_recovery_vault_path(), mfa_snapshot.as_deref())?
                || !snapshot_matches(
                    passwords.shared_recovery_vault_path(),
                    password_snapshot.as_deref(),
                )?
            {
                return Err(AppError::new(
                    "shared_recovery_vault_conflict",
                    "准备共享恢复密码事务时保险库已发生变化，请重试。",
                ));
            }
            let journal = SharedRecoveryJournal {
                schema_version: JOURNAL_SCHEMA_VERSION,
                transaction_id,
                state: SharedRecoveryJournalState::Prepared,
                mfa: mfa_record,
                passwords: password_record,
            };
            atomic_write_json(&journal_path, &journal)?;
            Ok(journal)
        })();
        let journal = match result {
            Ok(journal) => journal,
            Err(error) => {
                let _ = cleanup_stage(&stage_path);
                return Err(error);
            }
        };
        Ok(Self {
            recovery_root,
            stage_path,
            journal_path,
            journal,
        })
    }

    fn commit(&self) -> AppResult<()> {
        let mut committed = self.journal.clone();
        committed.state = SharedRecoveryJournalState::Committed;
        atomic_write_json(&self.journal_path, &committed)?;
        if cleanup_stage(&self.stage_path).is_ok() {
            remove_file_durable(&self.journal_path, "提交共享恢复密码事务")?;
        }
        Ok(())
    }

    fn rollback_disk(&self, mfa: &MfaStore, passwords: &PasswordStore) -> AppResult<()> {
        validate_runtime_paths(mfa, passwords, &self.recovery_root)?;
        let paths = RuntimeRecoveryPaths {
            mfa_vault: mfa.shared_recovery_vault_path().to_path_buf(),
            mfa_backups: mfa.shared_recovery_backup_path().to_path_buf(),
            password_vault: passwords.shared_recovery_vault_path().to_path_buf(),
            password_backups: passwords.shared_recovery_backup_path().to_path_buf(),
            stage_path: self.stage_path.clone(),
        };
        restore_from_journal(&self.journal, &paths)?;
        cleanup_stage(&self.stage_path)?;
        remove_file_durable(&self.journal_path, "完成共享恢复密码回滚")?;
        Ok(())
    }
}

struct RuntimeRecoveryPaths {
    mfa_vault: PathBuf,
    mfa_backups: PathBuf,
    password_vault: PathBuf,
    password_backups: PathBuf,
    stage_path: PathBuf,
}

fn recovery_paths(
    data_storage_path: &Path,
    recovery_root: &Path,
    journal: &SharedRecoveryJournal,
) -> AppResult<RuntimeRecoveryPaths> {
    let mfa_root =
        ensure_managed_subdirectory(data_storage_path, &[INTERNAL_DATA_DIR, "tools", "mfa"])?;
    let mfa_backups = ensure_managed_subdirectory(
        data_storage_path,
        &[INTERNAL_DATA_DIR, "tools", "mfa", "backups"],
    )?;
    let password_root = ensure_managed_subdirectory(
        data_storage_path,
        &[INTERNAL_DATA_DIR, "tools", "passwords"],
    )?;
    let password_backups = ensure_managed_subdirectory(
        data_storage_path,
        &[INTERNAL_DATA_DIR, "tools", "passwords", "backups"],
    )?;
    let stage_name = format!("shared-recovery-{}", journal.transaction_id);
    let stage_path = ensure_managed_subdirectory(recovery_root, &[stage_name.as_str()])?;
    Ok(RuntimeRecoveryPaths {
        mfa_vault: mfa_root.join("vault.json"),
        mfa_backups,
        password_vault: password_root.join("vault.json"),
        password_backups,
        stage_path,
    })
}

fn validate_runtime_paths(
    mfa: &MfaStore,
    passwords: &PasswordStore,
    recovery_root: &Path,
) -> AppResult<()> {
    let tools_root = recovery_root.parent().ok_or_else(|| {
        AppError::new("shared_recovery_path_invalid", "共享恢复密码事务目录无效。")
    })?;
    let valid = mfa.shared_recovery_vault_path() == tools_root.join("mfa").join("vault.json")
        && mfa.shared_recovery_backup_path() == tools_root.join("mfa").join("backups")
        && passwords.shared_recovery_vault_path()
            == tools_root.join("passwords").join("vault.json")
        && passwords.shared_recovery_backup_path() == tools_root.join("passwords").join("backups");
    if !valid {
        return Err(AppError::new(
            "shared_recovery_path_invalid",
            "MFA 与密码保险库不属于同一个飞花数据目录。",
        ));
    }
    Ok(())
}

fn write_snapshot(path: &Path, snapshot: Option<&[u8]>) -> AppResult<VaultSnapshotRecord> {
    match snapshot {
        Some(bytes) => {
            atomic_write(path, bytes)?;
            Ok(VaultSnapshotRecord {
                existed: true,
                byte_len: bytes.len() as u64,
                sha256: bytes_hash(bytes),
            })
        }
        None => Ok(VaultSnapshotRecord {
            existed: false,
            byte_len: 0,
            sha256: String::new(),
        }),
    }
}

fn snapshot_matches(path: &Path, expected: Option<&[u8]>) -> AppResult<bool> {
    match expected {
        Some(expected) => fs::read(path)
            .map(|bytes| bytes == expected)
            .map_err(|error| AppError::io("核对共享恢复密码保险库快照", error)),
        None => Ok(!path.exists()),
    }
}

fn read_journal(path: &Path) -> AppResult<SharedRecoveryJournal> {
    let metadata =
        fs::metadata(path).map_err(|error| AppError::io("读取共享恢复密码事务", error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(journal_invalid_error());
    }
    let bytes = fs::read(path).map_err(|error| AppError::io("读取共享恢复密码事务", error))?;
    let journal: SharedRecoveryJournal =
        serde_json::from_slice(&bytes).map_err(|_| journal_invalid_error())?;
    if journal.schema_version != JOURNAL_SCHEMA_VERSION
        || Uuid::parse_str(&journal.transaction_id)
            .ok()
            .is_none_or(|id| id.to_string() != journal.transaction_id)
    {
        return Err(journal_invalid_error());
    }
    validate_snapshot_record(&journal.mfa, MAX_MFA_SNAPSHOT_BYTES)?;
    validate_snapshot_record(&journal.passwords, MAX_PASSWORD_SNAPSHOT_BYTES)?;
    Ok(journal)
}

fn validate_snapshot_record(record: &VaultSnapshotRecord, maximum: u64) -> AppResult<()> {
    let valid_hash =
        record.sha256.len() == 64 && record.sha256.bytes().all(|byte| byte.is_ascii_hexdigit());
    let valid = if record.existed {
        record.byte_len > 0 && record.byte_len <= maximum && valid_hash
    } else {
        record.byte_len == 0 && record.sha256.is_empty()
    };
    if valid {
        Ok(())
    } else {
        Err(journal_invalid_error())
    }
}

fn restore_from_journal(
    journal: &SharedRecoveryJournal,
    paths: &RuntimeRecoveryPaths,
) -> AppResult<()> {
    let mfa = read_snapshot(
        &paths.stage_path.join(MFA_SNAPSHOT_FILE),
        &journal.mfa,
        MAX_MFA_SNAPSHOT_BYTES,
    )?;
    let passwords = read_snapshot(
        &paths.stage_path.join(PASSWORD_SNAPSHOT_FILE),
        &journal.passwords,
        MAX_PASSWORD_SNAPSHOT_BYTES,
    )?;
    restore_snapshot(
        &paths.mfa_vault,
        &paths.mfa_backups,
        mfa.as_deref(),
        &journal.transaction_id,
    )?;
    restore_snapshot(
        &paths.password_vault,
        &paths.password_backups,
        passwords.as_deref(),
        &journal.transaction_id,
    )?;
    Ok(())
}

fn read_snapshot(
    path: &Path,
    record: &VaultSnapshotRecord,
    maximum: u64,
) -> AppResult<Option<Vec<u8>>> {
    if !record.existed {
        return Ok(None);
    }
    let metadata = fs::metadata(path).map_err(|_| journal_invalid_error())?;
    if !metadata.is_file() || metadata.len() != record.byte_len || metadata.len() > maximum {
        return Err(journal_invalid_error());
    }
    let bytes = fs::read(path).map_err(|_| journal_invalid_error())?;
    if bytes_hash(&bytes) != record.sha256 {
        return Err(journal_invalid_error());
    }
    Ok(Some(bytes))
}

fn restore_snapshot(
    vault_path: &Path,
    backup_path: &Path,
    snapshot: Option<&[u8]>,
    transaction_id: &str,
) -> AppResult<()> {
    match snapshot {
        Some(bytes) => atomic_write(vault_path, bytes)?,
        None => match fs::remove_file(vault_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::io("回滚共享恢复密码保险库", error)),
        },
    }
    clear_json_backups(backup_path)?;
    if let Some(bytes) = snapshot {
        atomic_write(
            &backup_path.join(format!("recovered-{transaction_id}.json")),
            bytes,
        )?;
    }
    Ok(())
}

fn clear_json_backups(path: &Path) -> AppResult<()> {
    for entry in
        fs::read_dir(path).map_err(|error| AppError::io("读取共享恢复密码备份目录", error))?
    {
        let entry = entry.map_err(|error| AppError::io("读取共享恢复密码备份", error))?;
        if entry
            .file_type()
            .map_err(|error| AppError::io("检查共享恢复密码备份", error))?
            .is_file()
            && entry.path().extension().is_some_and(|ext| ext == "json")
        {
            fs::remove_file(entry.path())
                .map_err(|error| AppError::io("回滚共享恢复密码备份", error))?;
        }
    }
    Ok(())
}

fn remove_file_durable(path: &Path, action: &str) -> AppResult<()> {
    fs::remove_file(path).map_err(|error| AppError::io(action, error))?;
    if let Some(parent) = path.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

fn cleanup_stage(path: &Path) -> AppResult<()> {
    for file in [MFA_SNAPSHOT_FILE, PASSWORD_SNAPSHOT_FILE] {
        match fs::remove_file(path.join(file)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(AppError::io("清理共享恢复密码加密快照", error)),
        }
    }
    match fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io("清理共享恢复密码事务目录", error)),
    }
}

fn cleanup_orphan_stages(recovery_root: &Path, active: Option<&str>) -> AppResult<()> {
    let canonical_root = fs::canonicalize(recovery_root)
        .map_err(|error| AppError::io("检查共享恢复密码事务目录", error))?;
    for entry in fs::read_dir(recovery_root)
        .map_err(|error| AppError::io("读取共享恢复密码事务目录", error))?
    {
        let entry = entry.map_err(|error| AppError::io("读取共享恢复密码事务", error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(id) = name.strip_prefix("shared-recovery-") else {
            continue;
        };
        if active == Some(id)
            || Uuid::parse_str(id)
                .ok()
                .is_none_or(|uuid| uuid.to_string() != id)
        {
            continue;
        }
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| AppError::io("检查共享恢复密码事务", error))?
            .is_dir()
        {
            continue;
        }
        let canonical = fs::canonicalize(&path)
            .map_err(|error| AppError::io("检查共享恢复密码事务目录", error))?;
        if canonical.parent() != Some(canonical_root.as_path()) {
            continue;
        }
        cleanup_stage(&canonical)?;
    }
    Ok(())
}

fn bytes_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn journal_invalid_error() -> AppError {
    AppError::new(
        "shared_recovery_journal_invalid",
        "共享恢复密码事务记录损坏；为避免两个保险库分裂，飞花拒绝继续加载。",
    )
}

fn rollback_error(update: AppError, rollback: AppError) -> AppError {
    AppError::new(
        "shared_recovery_rollback_failed",
        "共享恢复密码更新中断，且无法自动恢复两个保险库，请立即保留数据目录并重启飞花。",
    )
    .with_details(serde_json::json!({
        "updateCode": update.code,
        "updateMessage": update.message,
        "rollbackCode": rollback.code,
        "rollbackMessage": rollback.message,
    }))
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const OLD_PASSWORD: &str = "shared-old-recovery-password";
    const NEW_PASSWORD: &str = "shared-new-recovery-password";

    #[test]
    fn first_password_setup_reuses_and_verifies_existing_mfa_password() {
        let root = tempdir().unwrap();
        let mfa = MfaStore::load(root.path()).unwrap();
        let passwords = PasswordStore::load(root.path()).unwrap();
        mfa.activate();
        passwords.activate();
        let initial_epoch = passwords.require_active_epoch().unwrap();
        let initial_status =
            annotate_password_status(&mfa, passwords.status_at(initial_epoch).unwrap()).unwrap();
        assert!(!initial_status.shared_recovery_configured);

        mfa.configure_shared_recovery_password(OLD_PASSWORD, None)
            .unwrap();

        let epoch = passwords.require_active_epoch().unwrap();
        let status = annotate_password_status(&mfa, passwords.status_at(epoch).unwrap()).unwrap();
        assert_eq!(
            status.recovery_state,
            crate::passwords::PasswordRecoveryState::SetupRequired
        );
        assert!(status.shared_recovery_configured);

        let wrong = configure_shared_recovery_password(
            &mfa,
            &passwords,
            "different-recovery-password",
            None,
        )
        .unwrap_err();
        assert_eq!(wrong.code, "mfa_recovery_password_invalid");
        assert!(!passwords.shared_recovery_is_configured_locked().unwrap());

        configure_shared_recovery_password(&mfa, &passwords, OLD_PASSWORD, None).unwrap();
        mfa.verify_shared_recovery_password(OLD_PASSWORD).unwrap();
        passwords
            .verify_shared_recovery_password(OLD_PASSWORD)
            .unwrap();

        // A stale setup dialog can submit after the other tool initialized
        // both vaults. The same password is idempotent; a different one fails.
        configure_shared_recovery_password(&mfa, &passwords, OLD_PASSWORD, None).unwrap();
        let different =
            configure_shared_recovery_password(&mfa, &passwords, NEW_PASSWORD, None).unwrap_err();
        assert_eq!(different.code, "mfa_recovery_password_invalid");
        mfa.verify_shared_recovery_password(OLD_PASSWORD).unwrap();
        passwords
            .verify_shared_recovery_password(OLD_PASSWORD)
            .unwrap();
    }

    #[test]
    fn second_vault_failure_rolls_the_first_vault_back_to_the_old_password() {
        let root = tempdir().unwrap();
        let mfa = MfaStore::load(root.path()).unwrap();
        let passwords = PasswordStore::load(root.path()).unwrap();
        mfa.activate();
        passwords.activate();
        configure_shared_recovery_password(&mfa, &passwords, OLD_PASSWORD, None).unwrap();

        let error = configure_shared_recovery_password_with(
            &mfa,
            &passwords,
            NEW_PASSWORD,
            Some(OLD_PASSWORD),
            |_, _, _| {
                Err(AppError::new(
                    "injected_password_vault_failure",
                    "injected second-vault failure",
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "injected_password_vault_failure");
        mfa.verify_shared_recovery_password(OLD_PASSWORD).unwrap();
        passwords
            .verify_shared_recovery_password(OLD_PASSWORD)
            .unwrap();
        assert!(mfa.verify_shared_recovery_password(NEW_PASSWORD).is_err());
    }

    #[test]
    fn interrupted_rotation_is_recovered_from_the_persistent_journal() {
        let root = tempdir().unwrap();
        let mfa = MfaStore::load(root.path()).unwrap();
        let passwords = PasswordStore::load(root.path()).unwrap();
        mfa.activate();
        passwords.activate();
        configure_shared_recovery_password(&mfa, &passwords, OLD_PASSWORD, None).unwrap();

        let transaction = SharedRecoveryTransaction::prepare(&mfa, &passwords).unwrap();
        mfa.configure_shared_recovery_password(NEW_PASSWORD, Some(OLD_PASSWORD))
            .unwrap();
        assert!(mfa.verify_shared_recovery_password(NEW_PASSWORD).is_ok());
        assert!(passwords
            .verify_shared_recovery_password(OLD_PASSWORD)
            .is_ok());
        let journal_bytes = fs::read(&transaction.journal_path).unwrap();
        assert!(!journal_bytes
            .windows(OLD_PASSWORD.len())
            .any(|window| window == OLD_PASSWORD.as_bytes()));
        assert!(!journal_bytes
            .windows(NEW_PASSWORD.len())
            .any(|window| window == NEW_PASSWORD.as_bytes()));
        drop(transaction);
        drop(passwords);
        drop(mfa);

        assert!(recover_interrupted_shared_recovery(root.path()).unwrap());
        assert!(!recover_interrupted_shared_recovery(root.path()).unwrap());
        let recovered_mfa = MfaStore::load(root.path()).unwrap();
        let recovered_passwords = PasswordStore::load(root.path()).unwrap();
        recovered_mfa
            .verify_shared_recovery_password(OLD_PASSWORD)
            .unwrap();
        recovered_passwords
            .verify_shared_recovery_password(OLD_PASSWORD)
            .unwrap();
        assert!(recovered_mfa
            .verify_shared_recovery_password(NEW_PASSWORD)
            .is_err());
        assert!(recovered_passwords
            .verify_shared_recovery_password(NEW_PASSWORD)
            .is_err());
    }

    #[test]
    fn either_window_can_coordinate_the_other_vault_while_it_is_inactive() {
        let root = tempdir().unwrap();
        let mfa = MfaStore::load(root.path()).unwrap();
        let passwords = PasswordStore::load(root.path()).unwrap();

        // The password window is the only active UI during first setup.
        passwords.activate();
        configure_shared_recovery_password(&mfa, &passwords, OLD_PASSWORD, None).unwrap();
        mfa.verify_shared_recovery_password(OLD_PASSWORD).unwrap();
        passwords
            .verify_shared_recovery_password(OLD_PASSWORD)
            .unwrap();

        // Then close the password window and rotate from the MFA window.
        passwords.lock();
        mfa.activate();
        configure_shared_recovery_password(&mfa, &passwords, NEW_PASSWORD, Some(OLD_PASSWORD))
            .unwrap();
        mfa.verify_shared_recovery_password(NEW_PASSWORD).unwrap();
        passwords
            .verify_shared_recovery_password(NEW_PASSWORD)
            .unwrap();
    }
}
