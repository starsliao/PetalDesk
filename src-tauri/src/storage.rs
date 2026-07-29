use crate::error::{AppError, AppResult};
use crate::models::*;
use chrono::{SecondsFormat, Utc};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use rusqlite::{params, Connection};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, RwLock, TryLockError};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

const MAX_ASSET_BYTES: usize = 25 * 1024 * 1024;
const BACKUP_LIMIT: usize = 5;
/// Snapshotting every autosave rewrote two files and rescanned the backup
/// directory several times per second while typing. Keep the safety net but
/// charge it at most once per interval per note.
const BACKUP_MIN_INTERVAL: Duration = Duration::from_secs(180);
const DATA_CONFIG_SCHEMA_VERSION: u32 = 1;
const NOTE_ORDER_SCHEMA_VERSION: u32 = 1;
const STORAGE_POINTER_FILE: &str = "storage-path.txt";
pub(crate) const INTERNAL_DATA_DIR: &str = ".petaldesk";
const LOCAL_APP_DATA_DIR: &str = "PetalDesk";
const DEFAULT_WORKSPACE_DIR: &str = "PetalDesk";
const LEGACY_INTERNAL_DATA_DIR: &str = concat!(".fei", "hua");
const LEGACY_LOCAL_APP_DATA_DIR: &str = concat!("Fei", "Hua");
const LEGACY_DEFAULT_WORKSPACE_DIR: &str = concat!("飞", "花");
const LEGACY_MIGRATION_MARKER_FILE: &str = "migration-v1.complete";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataStorageConfig {
    #[serde(default = "data_config_schema_version")]
    schema_version: u32,
    #[serde(default = "default_editor_mode", alias = "editorMode")]
    default_editor_mode: String,
}

impl Default for DataStorageConfig {
    fn default() -> Self {
        Self {
            schema_version: DATA_CONFIG_SCHEMA_VERSION,
            default_editor_mode: DEFAULT_EDITOR_MODE.to_string(),
        }
    }
}

fn data_config_schema_version() -> u32 {
    DATA_CONFIG_SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredNoteOrder {
    #[serde(default = "note_order_schema_version")]
    schema_version: u32,
    #[serde(default)]
    ordered_ids: Vec<String>,
}

fn note_order_schema_version() -> u32 {
    NOTE_ORDER_SCHEMA_VERSION
}

/// Cheap identity of `note.md` on disk. Comparing this avoids reading and
/// hashing note bodies on every external-change poll.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }

    /// A stamp without a usable mtime cannot prove the file is unchanged, so
    /// such entries always fall through to a full content comparison.
    fn is_trustworthy(&self) -> bool {
        self.modified.is_some()
    }
}

pub struct WorkspaceStore {
    workspace: RwLock<PathBuf>,
    default_editor_mode: RwLock<String>,
    app_data: PathBuf,
    startup_recovery: RwLock<Vec<RecoveredDraft>>,
    mutation_lock: Mutex<()>,
    note_order_lock: Mutex<()>,
    /// `note.md` stamps already known to match the recorded `contentHash`.
    external_scan_cache: Mutex<HashMap<String, FileStamp>>,
    /// Last time each note was snapshotted into `backups/`.
    backup_clock: Mutex<HashMap<String, Instant>>,
    /// Long-lived FTS connection. Reopening it per write re-ran the pragmas and
    /// the `CREATE VIRTUAL TABLE` probe every time.
    index_connection: Mutex<Option<Connection>>,
}

impl WorkspaceStore {
    pub(crate) const TIMER_MIN_WIDTH: f64 = 100.0;
    pub(crate) const TIMER_MIN_HEIGHT: f64 = 50.0;
    pub(crate) const TIMER_MAX_WIDTH: f64 = 320.0;
    pub(crate) const TIMER_MAX_HEIGHT: f64 = 194.0;
    pub(crate) const GANTT_MIN_WIDTH: f64 = 680.0;
    pub(crate) const GANTT_MIN_HEIGHT: f64 = 400.0;

    pub fn load() -> AppResult<Self> {
        let local_data_root = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
        let app_data = local_data_root.join(LOCAL_APP_DATA_DIR);
        let legacy_app_data = local_data_root.join(LEGACY_LOCAL_APP_DATA_DIR);
        fs::create_dir_all(&app_data).map_err(|error| AppError::io("创建应用数据目录", error))?;
        let document_root = dirs::document_dir().unwrap_or_else(|| app_data.clone());
        let (configured, stored_settings) =
            resolve_workspace_configuration(&app_data, &legacy_app_data, &document_root)?;
        let workspace = prepare_workspace(&configured)?;
        let legacy_default_workspace = document_root.join(LEGACY_DEFAULT_WORKSPACE_DIR);
        let current_default_workspace = document_root.join(DEFAULT_WORKSPACE_DIR);
        let legacy_workspace = (paths_refer_same(&workspace, &current_default_workspace)
            && legacy_workspace_has_data(&legacy_default_workspace))
        .then_some(legacy_default_workspace.as_path());
        migrate_legacy_storage(
            &workspace,
            legacy_workspace,
            &app_data,
            Some(&legacy_app_data),
            stored_settings.as_ref(),
        )?;
        let default_editor_mode = load_data_storage_config(&workspace, stored_settings.as_ref())?;
        let store = Self {
            workspace: RwLock::new(workspace),
            default_editor_mode: RwLock::new(default_editor_mode),
            app_data,
            startup_recovery: RwLock::new(Vec::new()),
            mutation_lock: Mutex::new(()),
            note_order_lock: Mutex::new(()),
            external_scan_cache: Mutex::new(HashMap::new()),
            backup_clock: Mutex::new(HashMap::new()),
            index_connection: Mutex::new(None),
        };
        store.save_data_storage_config()?;
        store.save_storage_pointer()?;
        *store
            .startup_recovery
            .write()
            .expect("startup recovery lock poisoned") = store.recover_journals()?;
        store.rebuild_index()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn for_test(workspace: &Path, app_data: &Path) -> AppResult<Self> {
        fs::create_dir_all(app_data).map_err(|error| AppError::io("创建测试应用目录", error))?;
        Ok(Self {
            workspace: RwLock::new(prepare_workspace(workspace)?),
            default_editor_mode: RwLock::new(DEFAULT_EDITOR_MODE.to_string()),
            app_data: app_data.to_path_buf(),
            startup_recovery: RwLock::new(Vec::new()),
            mutation_lock: Mutex::new(()),
            note_order_lock: Mutex::new(()),
            external_scan_cache: Mutex::new(HashMap::new()),
            backup_clock: Mutex::new(HashMap::new()),
            index_connection: Mutex::new(None),
        })
    }

    pub fn workspace_path(&self) -> PathBuf {
        self.workspace
            .read()
            .expect("workspace path lock poisoned")
            .clone()
    }

    pub fn data_storage_path(&self) -> PathBuf {
        self.workspace_path()
    }

    pub fn default_editor_mode(&self) -> String {
        self.default_editor_mode
            .read()
            .expect("editor mode lock poisoned")
            .clone()
    }

    pub fn set_default_editor_mode(&self, editor_mode: &str) -> AppResult<String> {
        if !ALLOWED_EDITOR_MODES.contains(&editor_mode) {
            return Err(AppError::invalid("不支持的编辑样式"));
        }

        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        *self
            .default_editor_mode
            .write()
            .expect("editor mode lock poisoned") = editor_mode.to_string();
        self.save_data_storage_config()?;
        Ok(editor_mode.to_string())
    }

    pub fn startup_recovery(&self) -> Vec<RecoveredDraft> {
        self.startup_recovery
            .read()
            .expect("startup recovery lock poisoned")
            .clone()
    }

    pub fn set_data_storage_path(&self, path: String) -> AppResult<DataStorageChangeResult> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        if path.trim().is_empty() {
            return Err(AppError::invalid("飞花 - PetalDesk 数据存储路径不能为空"));
        }
        let prepared = prepare_workspace(Path::new(path.trim()))?;
        let config_path = data_storage_config_path(&prepared);
        if !config_path.exists() {
            atomic_write_json(
                &config_path,
                &DataStorageConfig {
                    schema_version: DATA_CONFIG_SCHEMA_VERSION,
                    default_editor_mode: self.default_editor_mode(),
                },
            )?;
        }
        write_storage_pointer(&self.app_data.join(STORAGE_POINTER_FILE), &prepared)?;
        let restart_required = prepared != self.workspace_path();
        Ok(DataStorageChangeResult {
            path: prepared.to_string_lossy().into_owned(),
            restart_required,
        })
    }

    pub fn list_notes(&self) -> AppResult<Vec<NoteSummary>> {
        let _order = self
            .note_order_lock
            .lock()
            .expect("note order lock poisoned");
        self.list_notes_with_order_locked()
    }

    fn list_notes_with_order_locked(&self) -> AppResult<Vec<NoteSummary>> {
        let (notes, directory_ids) = self.scan_notes()?;
        let ordered_ids = self.reconcile_note_order_locked(&notes, &directory_ids)?;
        Ok(notes_in_order(notes, &ordered_ids))
    }

    fn reconcile_note_order_locked(
        &self,
        notes: &[NoteSummary],
        active_ids: &HashSet<String>,
    ) -> AppResult<Vec<String>> {
        let path = self.note_order_path();
        let mut changed = false;
        let mut ordered_ids = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoredNoteOrder>(&bytes) {
                Ok(stored) => {
                    if stored.schema_version != NOTE_ORDER_SCHEMA_VERSION {
                        changed = true;
                    }
                    let stored_len = stored.ordered_ids.len();
                    let mut seen = HashSet::new();
                    let filtered = stored
                        .ordered_ids
                        .into_iter()
                        .filter(|id| active_ids.contains(id) && seen.insert(id.clone()))
                        .collect::<Vec<_>>();
                    if filtered.len() != stored_len || filtered.len() != active_ids.len() {
                        changed = true;
                    }
                    filtered
                }
                Err(_) => {
                    preserve_corrupt_note_order(&path);
                    changed = true;
                    legacy_note_order(notes)
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                changed = true;
                legacy_note_order(notes)
            }
            Err(error) => return Err(AppError::io("读取便签顺序", error)),
        };

        let known_ids = ordered_ids.iter().cloned().collect::<HashSet<_>>();
        let notes_by_id = notes
            .iter()
            .map(|note| (note.id.as_str(), note))
            .collect::<HashMap<_, _>>();
        let mut missing = active_ids
            .iter()
            .filter(|id| !known_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        missing.sort_by(|left, right| {
            match (
                notes_by_id.get(left.as_str()),
                notes_by_id.get(right.as_str()),
            ) {
                (Some(left_note), Some(right_note)) => left_note
                    .created_at
                    .cmp(&right_note.created_at)
                    .then_with(|| left.cmp(right)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.cmp(right),
            }
        });
        if !missing.is_empty() {
            changed = true;
            ordered_ids.extend(missing);
        }

        if changed {
            self.save_note_order_locked(&ordered_ids)?;
        }
        Ok(ordered_ids)
    }

    fn save_note_order_locked(&self, ordered_ids: &[String]) -> AppResult<()> {
        atomic_write_json(
            &self.note_order_path(),
            &StoredNoteOrder {
                schema_version: NOTE_ORDER_SCHEMA_VERSION,
                ordered_ids: ordered_ids.to_vec(),
            },
        )
    }

    fn refresh_note_order_locked(&self) -> AppResult<Vec<String>> {
        let (notes, directory_ids) = self.scan_notes()?;
        self.reconcile_note_order_locked(&notes, &directory_ids)
    }

    fn scan_notes(&self) -> AppResult<(Vec<NoteSummary>, HashSet<String>)> {
        let mut notes = Vec::new();
        let mut directory_ids = HashSet::new();
        for entry in
            fs::read_dir(self.notes_dir()).map_err(|error| AppError::io("读取便签目录", error))?
        {
            let entry = entry.map_err(|error| AppError::io("读取便签条目", error))?;
            if !entry
                .file_type()
                .map_err(|error| AppError::io("读取便签类型", error))?
                .is_dir()
            {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if validate_note_id(&id).is_err() {
                continue;
            }
            directory_ids.insert(id.clone());
            if let Ok(snapshot) = self.read_snapshot(&id, true) {
                notes.push(summary_from_snapshot(&snapshot));
            }
        }
        Ok((notes, directory_ids))
    }

    pub fn reorder_notes(&self, ordered_ids: Vec<String>) -> AppResult<Vec<NoteSummary>> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        let _order = self
            .note_order_lock
            .lock()
            .expect("note order lock poisoned");
        let (notes, directory_ids) = self.scan_notes()?;
        let active_ids = notes
            .iter()
            .map(|note| note.id.clone())
            .collect::<HashSet<_>>();
        if active_ids != directory_ids {
            return Err(AppError::new(
                "notes_temporarily_unavailable",
                "部分便签暂时无法读取，请稍后再调整顺序",
            ));
        }
        let mut seen = HashSet::new();
        for id in &ordered_ids {
            validate_note_id(id)?;
            if !seen.insert(id.as_str()) {
                return Err(AppError::invalid("便签顺序中不能包含重复便签"));
            }
            if !active_ids.contains(id) {
                return Err(AppError::invalid("便签顺序中包含不存在的便签"));
            }
        }
        if seen.len() != active_ids.len() {
            return Err(AppError::invalid("便签顺序必须包含当前全部便签"));
        }
        self.save_note_order_locked(&ordered_ids)?;
        Ok(notes_in_order(notes, &ordered_ids))
    }

    pub fn create_note(&self) -> AppResult<NoteSnapshot> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        let note_uuid = Uuid::new_v4();
        let id = note_uuid.to_string();
        let note_dir = self.note_dir(&id)?;
        fs::create_dir_all(note_dir.join("assets"))
            .map_err(|error| AppError::io("创建便签目录", error))?;
        let now = now();
        let markdown = String::new();
        let meta = NoteMeta {
            id: id.clone(),
            title: DEFAULT_NOTE_TITLE.to_string(),
            editor_mode: self.default_editor_mode(),
            color: note_color_for_entropy(note_uuid.as_u128()).to_string(),
            pinned: false,
            read_only: false,
            created_at: now.clone(),
            updated_at: now,
            schema_version: SCHEMA_VERSION,
            revision: 0,
            content_hash: content_hash(markdown.as_bytes()),
        };
        atomic_write(&note_dir.join("note.md"), markdown.as_bytes())?;
        atomic_write_json(&note_dir.join("meta.json"), &meta)?;
        {
            let _order = self
                .note_order_lock
                .lock()
                .expect("note order lock poisoned");
            self.refresh_note_order_locked()?;
        }
        self.index_note(&id, &markdown, &meta)?;
        Ok(NoteSnapshot {
            id,
            revision: 0,
            markdown,
            meta,
        })
    }

    pub fn get_note(&self, id: &str) -> AppResult<NoteSnapshot> {
        let snapshot = self.read_snapshot(id, true)?;
        self.index_note(id, &snapshot.markdown, &snapshot.meta)?;
        Ok(snapshot)
    }

    pub fn commit_note(&self, request: CommitNoteRequest) -> AppResult<CommitResult> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        validate_note_id(&request.id)?;
        validate_color_patch(request.meta_patch.color.as_deref())?;
        validate_editor_mode_patch(request.meta_patch.editor_mode.as_deref())?;
        let current = self.read_snapshot(&request.id, true)?;
        if request.base_revision != current.revision {
            let conflict_path = self.write_conflict_copy(&request.id, &request.markdown)?;
            return Err(
                AppError::new("revision_conflict", "便签已在其他位置修改").with_details(json!({
                    "expectedRevision": request.base_revision,
                    "actualRevision": current.revision,
                    "conflictPath": conflict_path.to_string_lossy(),
                })),
            );
        }
        let move_to_front = request.meta_patch.pinned == Some(true) && !current.meta.pinned;
        let previous_order = if move_to_front {
            let _order = self
                .note_order_lock
                .lock()
                .expect("note order lock poisoned");
            let previous = self.refresh_note_order_locked()?;
            let mut next = previous.clone();
            next.retain(|id| id != &request.id);
            next.insert(0, request.id.clone());
            self.save_note_order_locked(&next)?;
            Some(previous)
        } else {
            None
        };

        let note_dir = self.note_dir(&request.id)?;
        let persisted = (|| -> AppResult<(String, NoteMeta, PathBuf)> {
            self.backup_current_throttled(&current)?;
            let saved_at = now();
            let mut meta = current.meta;
            if let Some(title) = request.meta_patch.title {
                meta.title = normalize_title(&title);
            }
            if let Some(editor_mode) = request.meta_patch.editor_mode {
                meta.editor_mode = editor_mode;
            }
            if let Some(color) = request.meta_patch.color {
                meta.color = color;
            }
            if let Some(pinned) = request.meta_patch.pinned {
                meta.pinned = pinned;
            }
            if let Some(read_only) = request.meta_patch.read_only {
                meta.read_only = read_only;
            }
            meta.revision = meta.revision.saturating_add(1);
            meta.updated_at = saved_at.clone();
            meta.content_hash = content_hash(request.markdown.as_bytes());
            let journal = JournalEntry {
                note_id: request.id.clone(),
                base_revision: request.base_revision,
                new_revision: meta.revision,
                markdown: request.markdown.clone(),
                meta: meta.clone(),
                created_at: saved_at.clone(),
            };
            let journal_path = self.journal_path(&request.id)?;
            atomic_write_json(&journal_path, &journal)?;
            atomic_write(&note_dir.join("note.md"), request.markdown.as_bytes())?;
            atomic_write_json(&note_dir.join("meta.json"), &meta)?;
            Ok((saved_at, meta, journal_path))
        })();
        let (saved_at, meta, journal_path) = match persisted {
            Ok(persisted) => persisted,
            Err(error) => {
                if let Some(previous_order) = previous_order {
                    let _order = self
                        .note_order_lock
                        .lock()
                        .expect("note order lock poisoned");
                    let _ = self.save_note_order_locked(&previous_order);
                }
                return Err(error);
            }
        };
        remove_file_if_exists(&journal_path)?;
        self.index_note(&request.id, &request.markdown, &meta)?;
        // Our own write is by definition in sync with the hash we just stored,
        // so stamp it now instead of letting the poller re-read and re-hash it.
        self.remember_clean_note(&request.id, &note_dir.join("note.md"));
        Ok(CommitResult {
            revision: meta.revision,
            saved_at,
        })
    }

    pub fn delete_note(&self, id: &str) -> AppResult<()> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        let source = self.note_dir(id)?;
        if !source.is_dir() {
            return Err(AppError::not_found("便签不存在"));
        }
        let destination = self.trash_dir().join(id);
        if destination.exists() {
            return Err(AppError::new("trash_conflict", "回收站中已有同名便签"));
        }
        fs::rename(&source, &destination)
            .map_err(|error| AppError::io("移动便签到回收站", error))?;
        {
            let _order = self
                .note_order_lock
                .lock()
                .expect("note order lock poisoned");
            self.refresh_note_order_locked()?;
        }
        self.with_index(|connection| {
            connection.execute("DELETE FROM note_search WHERE id = ?1", params![id])?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn list_trash(&self) -> AppResult<Vec<NoteSummary>> {
        let mut notes = Vec::new();
        for entry in
            fs::read_dir(self.trash_dir()).map_err(|error| AppError::io("读取回收站", error))?
        {
            let entry = entry.map_err(|error| AppError::io("读取回收站条目", error))?;
            if !entry.path().is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if validate_note_id(&id).is_err() {
                continue;
            }
            let markdown = fs::read_to_string(entry.path().join("note.md")).unwrap_or_default();
            let meta_path = entry.path().join("meta.json");
            if let Ok(mut meta) = read_json::<NoteMeta>(&meta_path) {
                if migrate_note_meta(&mut meta, &markdown) {
                    atomic_write_json(&meta_path, &meta)?;
                }
                notes.push(summary_from_snapshot(&NoteSnapshot {
                    id: id.clone(),
                    revision: meta.revision,
                    markdown,
                    meta,
                }));
            }
        }
        notes.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(notes)
    }

    pub fn restore_note(&self, id: &str) -> AppResult<NoteSnapshot> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        validate_note_id(id)?;
        let source = self.trash_dir().join(id);
        if !source.is_dir() {
            return Err(AppError::not_found("回收站中没有该便签"));
        }
        let destination = self.note_dir(id)?;
        if destination.exists() {
            return Err(AppError::new(
                "note_conflict",
                "当前飞花 - PetalDesk 数据存储中已有同名便签",
            ));
        }
        fs::rename(&source, &destination).map_err(|error| AppError::io("恢复便签", error))?;
        let snapshot = self.read_snapshot(id, true)?;
        {
            let _order = self
                .note_order_lock
                .lock()
                .expect("note order lock poisoned");
            self.refresh_note_order_locked()?;
        }
        self.index_note(id, &snapshot.markdown, &snapshot.meta)?;
        Ok(snapshot)
    }

    pub fn empty_trash(&self) -> AppResult<()> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        let trash = self.trash_dir();
        if trash.exists() {
            for entry in fs::read_dir(&trash).map_err(|error| AppError::io("读取回收站", error))?
            {
                let path = entry
                    .map_err(|error| AppError::io("读取回收站条目", error))?
                    .path();
                if path.is_dir() {
                    fs::remove_dir_all(&path)
                        .map_err(|error| AppError::io("清空回收站目录", error))?;
                } else {
                    fs::remove_file(&path)
                        .map_err(|error| AppError::io("清空回收站文件", error))?;
                }
            }
        }
        Ok(())
    }

    pub fn import_asset(
        &self,
        note_id: &str,
        file_name: &str,
        bytes: &[u8],
    ) -> AppResult<AssetResult> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        validate_note_id(note_id)?;
        if bytes.is_empty() || bytes.len() > MAX_ASSET_BYTES {
            return Err(AppError::invalid("图片必须大于 0 字节且不超过 25 MB"));
        }
        if Path::new(file_name).file_name() != Some(OsStr::new(file_name)) {
            return Err(AppError::invalid("图片文件名不能包含路径"));
        }
        let kind = infer::get(bytes).ok_or_else(|| AppError::invalid("无法识别图片格式"))?;
        let (extension, accepted) = match kind.mime_type() {
            "image/png" => ("png", true),
            "image/jpeg" => ("jpg", true),
            "image/gif" => ("gif", true),
            "image/webp" => ("webp", true),
            "image/bmp" => ("bmp", true),
            "image/avif" => ("avif", true),
            _ => ("", false),
        };
        if !accepted {
            return Err(AppError::invalid(
                "仅支持 PNG、JPEG、GIF、WebP、BMP 和 AVIF 图片",
            ));
        }
        let note_dir = self.note_dir(note_id)?;
        if !note_dir.join("meta.json").is_file() || !note_dir.join("note.md").is_file() {
            return Err(AppError::not_found("便签不存在"));
        }
        let asset_id = content_hash(bytes);
        let file = format!("{asset_id}.{extension}");
        let assets_dir = note_dir.join("assets");
        fs::create_dir_all(&assets_dir).map_err(|error| AppError::io("创建图片目录", error))?;
        let destination = assets_dir.join(&file);
        if !destination.exists() {
            atomic_write(&destination, bytes)?;
        }
        Ok(AssetResult {
            relative_path: format!("assets/{file}"),
            asset_id,
        })
    }

    pub fn read_asset(&self, note_id: &str, relative_path: &str) -> AppResult<AssetContent> {
        let path = self.safe_asset_path(note_id, relative_path)?;
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AppError::not_found("图片不存在")
            } else {
                AppError::io("读取图片", error)
            }
        })?;
        let mime = infer::get(&bytes)
            .map(|kind| kind.mime_type().to_string())
            .or_else(|| mime_guess::from_path(&path).first_raw().map(str::to_string))
            .filter(|mime| mime.starts_with("image/"))
            .ok_or_else(|| AppError::invalid("资源不是受支持的图片"))?;
        Ok(AssetContent { mime, bytes })
    }

    pub fn search_notes(&self, query: &str, limit: Option<u32>) -> AppResult<Vec<SearchResult>> {
        let query = query.trim();
        let limit = limit.unwrap_or(50).clamp(1, 200);
        if query.is_empty() {
            return Ok(self
                .list_notes()?
                .into_iter()
                .map(|note| SearchResult {
                    snippet: note.excerpt.clone(),
                    note,
                })
                .take(limit as usize)
                .collect());
        }
        let indexed_rows = match self.query_index(query, limit) {
            Ok(rows) => rows,
            Err(_) => {
                remove_sqlite_files(&self.index_path());
                self.rebuild_index()?;
                self.query_index(query, limit)?
            }
        };
        let mut results = Vec::new();
        for (id, snippet) in indexed_rows {
            if let Ok(snapshot) = self.read_snapshot(&id, true) {
                results.push(SearchResult {
                    note: summary_from_snapshot(&snapshot),
                    snippet,
                });
            }
        }
        Ok(results)
    }

    fn query_index(&self, query: &str, limit: u32) -> AppResult<Vec<(String, String)>> {
        let match_query = query
            .split_whitespace()
            .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let like_query = format!("%{query}%");
        self.with_index(move |connection| {
            let mut results = {
                let mut statement = connection.prepare_cached(
                    "SELECT id, snippet(note_search, 2, '', '', '…', 24) \
                     FROM note_search \
                     WHERE note_search MATCH ?1 \
                     ORDER BY rank, updated_at DESC LIMIT ?2",
                )?;
                let rows = statement.query_map(params![match_query, limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            if !results.is_empty() {
                return Ok(results);
            }

            let mut statement = connection.prepare_cached(
                "SELECT id, substr(body, 1, 160) \
                 FROM note_search \
                 WHERE title LIKE ?1 OR body LIKE ?1 \
                 ORDER BY updated_at DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![like_query, limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
    }

    pub fn save_window_state(&self, label: &str, state: WindowState) -> AppResult<()> {
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        validate_window_label(label)?;
        let state = Self::normalize_window_state(label, state)
            .ok_or_else(|| AppError::invalid("窗口位置或尺寸无效"))?;
        let path = self.windows_state_path();
        let mut states = read_json::<StoredWindowStates>(&path).unwrap_or_default();
        states.windows.insert(label.to_string(), state);
        atomic_write_json(&path, &states)
    }

    pub fn window_state(&self, label: &str) -> Option<WindowState> {
        read_json::<StoredWindowStates>(&self.windows_state_path())
            .ok()
            .and_then(|states| states.windows.get(label).cloned())
            .and_then(|state| Self::normalize_window_state(label, state))
    }

    fn normalize_window_state(label: &str, mut state: WindowState) -> Option<WindowState> {
        let (min_width, min_height) = match label {
            "timer" => (Self::TIMER_MIN_WIDTH, Self::TIMER_MIN_HEIGHT),
            "reminder" => (440.0, 360.0),
            "gantt" => (Self::GANTT_MIN_WIDTH, Self::GANTT_MIN_HEIGHT),
            _ => (240.0, 160.0),
        };
        let valid = state.x.is_finite()
            && state.y.is_finite()
            && state.width.is_finite()
            && state.height.is_finite()
            && state.width >= min_width
            && state.height >= min_height;
        if !valid {
            return None;
        }
        if label == "timer" {
            state.width = state.width.min(Self::TIMER_MAX_WIDTH);
            state.height = state.height.min(Self::TIMER_MAX_HEIGHT);
            state.maximized = false;
        }
        Some(state)
    }

    pub fn set_note_window_open(&self, id: &str, open: bool) -> AppResult<()> {
        validate_note_id(id)?;
        let _mutation = self
            .mutation_lock
            .lock()
            .expect("workspace mutation lock poisoned");
        let path = self.windows_state_path();
        let mut states = read_json::<StoredWindowStates>(&path).unwrap_or_default();
        states.open_notes.retain(|note_id| note_id != id);
        if open {
            states.open_notes.push(id.to_string());
        }
        states.last_note_id = Some(id.to_string());
        atomic_write_json(&path, &states)
    }

    pub fn last_or_recent_note_id(&self) -> AppResult<Option<String>> {
        let states =
            read_json::<StoredWindowStates>(&self.windows_state_path()).unwrap_or_default();
        if let Some(id) = states.last_note_id {
            if validate_note_id(&id).is_ok() && self.note_dir(&id).is_ok_and(|path| path.is_dir()) {
                return Ok(Some(id));
            }
        }

        Ok(self
            .list_notes()?
            .into_iter()
            .max_by(|left, right| {
                left.updated_at
                    .cmp(&right.updated_at)
                    .then_with(|| left.created_at.cmp(&right.created_at))
                    .then_with(|| left.id.cmp(&right.id))
            })
            .map(|note| note.id))
    }

    pub fn first_note_id(&self) -> AppResult<Option<String>> {
        Ok(self.list_notes()?.into_iter().next().map(|note| note.id))
    }

    pub fn detect_external_changes(&self) -> AppResult<Vec<NoteSnapshot>> {
        // Directory traversal and ordinary file reads can be delayed by cloud
        // sync, antivirus or a removable drive. Do that work without holding
        // the foreground mutation lock, then re-check only the candidates once
        // the lock is available.
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        // Freshly observed clean stamps, merged into the shared cache at the end
        // so the scan itself never holds the cache lock across file I/O.
        let mut clean = Vec::new();
        for entry in fs::read_dir(self.notes_dir())
            .map_err(|error| AppError::io("检测外部便签修改", error))?
        {
            let entry = entry.map_err(|error| AppError::io("读取便签条目", error))?;
            let note_path = entry.path();
            if !note_path.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if validate_note_id(&id).is_err() {
                continue;
            }
            seen.insert(id.clone());

            // Fast path: an unchanged mtime/size pair means the body still
            // matches the hash we verified earlier, so skip the read entirely.
            let markdown_path = note_path.join("note.md");
            let stamp = FileStamp::of(&markdown_path);
            if let Some(stamp) = stamp {
                if stamp.is_trustworthy() {
                    let cached = self
                        .external_scan_cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&id)
                        .copied();
                    if cached == Some(stamp) {
                        continue;
                    }
                }
            }

            let meta = match read_json::<NoteMeta>(&note_path.join("meta.json")) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            let markdown = match fs::read(&markdown_path) {
                Ok(markdown) => markdown,
                Err(_) => continue,
            };
            if meta.content_hash == content_hash(&markdown) {
                // Re-stamp after reading so a write that landed mid-read is not
                // mistaken for clean on the next poll.
                if let Some(stamp) = FileStamp::of(&markdown_path) {
                    if stamp.is_trustworthy() && Some(stamp) == FileStamp::of(&markdown_path) {
                        clean.push((id, stamp));
                    }
                }
                continue;
            }
            candidates.push(id);
        }

        {
            let mut cache = self
                .external_scan_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.retain(|id, _| seen.contains(id));
            cache.extend(clean);
            // Deleted notes must not linger, and changed ones get re-stamped
            // only once their new content has been indexed below.
            for id in &candidates {
                cache.remove(id);
            }
        }

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let _mutation = match self.mutation_lock.try_lock() {
            Ok(mutation) => mutation,
            Err(TryLockError::WouldBlock) => return Ok(Vec::new()),
            Err(TryLockError::Poisoned(_)) => panic!("workspace mutation lock poisoned"),
        };
        let mut changed = Vec::new();
        for id in candidates {
            let note_dir = self.note_dir(&id)?;
            let markdown_path = note_dir.join("note.md");
            let meta = match read_json::<NoteMeta>(&note_dir.join("meta.json")) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            let markdown = match fs::read(&markdown_path) {
                Ok(markdown) => markdown,
                Err(_) => continue,
            };
            if meta.content_hash == content_hash(&markdown) {
                continue;
            }
            let snapshot = self.read_snapshot(&id, true)?;
            self.index_note(&id, &snapshot.markdown, &snapshot.meta)?;
            // `read_snapshot` rewrote `contentHash` to match the file, so the
            // current stamp is a valid clean marker for later polls.
            self.remember_clean_note(&id, &markdown_path);
            changed.push(snapshot);
        }
        Ok(changed)
    }

    pub fn recover_journals(&self) -> AppResult<Vec<RecoveredDraft>> {
        let mut recovered = Vec::new();
        for entry in
            fs::read_dir(self.journal_dir()).map_err(|error| AppError::io("读取恢复日志", error))?
        {
            let entry = entry.map_err(|error| AppError::io("读取恢复日志条目", error))?;
            let path = entry.path();
            if path.extension().and_then(OsStr::to_str) != Some("json") {
                continue;
            }
            let mut journal = match read_json::<JournalEntry>(&path) {
                Ok(journal) => journal,
                Err(_) => {
                    let destination =
                        path.with_extension(format!("corrupt-{}.json", Uuid::new_v4()));
                    fs::rename(&path, &destination)
                        .map_err(|error| AppError::io("隔离损坏的恢复日志", error))?;
                    recovered.push(RecoveredDraft {
                        note_id: entry.file_name().to_string_lossy().into_owned(),
                        status: "corrupt".to_string(),
                        recovered_path: Some(destination.to_string_lossy().into_owned()),
                    });
                    continue;
                }
            };
            validate_note_id(&journal.note_id)?;
            migrate_note_meta(&mut journal.meta, &journal.markdown);
            let note_dir = self.note_dir(&journal.note_id)?;
            fs::create_dir_all(note_dir.join("assets"))
                .map_err(|error| AppError::io("创建恢复便签目录", error))?;
            let existing_revision = read_json::<NoteMeta>(&note_dir.join("meta.json"))
                .map(|meta| meta.revision)
                .unwrap_or(0);
            if existing_revision <= journal.base_revision {
                atomic_write(&note_dir.join("note.md"), journal.markdown.as_bytes())?;
                atomic_write_json(&note_dir.join("meta.json"), &journal.meta)?;
                recovered.push(RecoveredDraft {
                    note_id: journal.note_id.clone(),
                    status: "restored".to_string(),
                    recovered_path: None,
                });
            } else {
                recovered.push(RecoveredDraft {
                    note_id: journal.note_id.clone(),
                    status: "alreadyCommitted".to_string(),
                    recovered_path: None,
                });
            }
            remove_file_if_exists(&path)?;
        }
        Ok(recovered)
    }

    fn read_snapshot(&self, id: &str, detect_external_change: bool) -> AppResult<NoteSnapshot> {
        let note_dir = self.note_dir(id)?;
        if !note_dir.is_dir() {
            return Err(AppError::not_found("便签不存在"));
        }
        let markdown = fs::read_to_string(note_dir.join("note.md"))
            .map_err(|error| AppError::io("读取便签正文", error))?;
        let mut meta: NoteMeta = read_json(&note_dir.join("meta.json"))?;
        if meta.id != id {
            return Err(AppError::new("invalid_data", "便签元数据 ID 不匹配"));
        }
        validate_color_patch(Some(&meta.color))?;
        let mut meta_changed = migrate_note_meta(&mut meta, &markdown);
        let actual_hash = content_hash(markdown.as_bytes());
        if detect_external_change && meta.content_hash != actual_hash {
            self.backup_current(&NoteSnapshot {
                id: id.to_string(),
                revision: meta.revision,
                markdown: markdown.clone(),
                meta: meta.clone(),
            })?;
            meta.revision = meta.revision.saturating_add(1);
            meta.updated_at = now();
            meta.content_hash = actual_hash;
            meta_changed = true;
        }
        if meta_changed {
            atomic_write_json(&note_dir.join("meta.json"), &meta)?;
        }
        Ok(NoteSnapshot {
            id: id.to_string(),
            revision: meta.revision,
            markdown,
            meta,
        })
    }

    /// Records the on-disk stamp of a note whose body is known to match its
    /// recorded hash, letting later polls skip the read entirely.
    fn remember_clean_note(&self, id: &str, markdown_path: &Path) {
        let Some(stamp) = FileStamp::of(markdown_path) else {
            return;
        };
        if !stamp.is_trustworthy() {
            return;
        }
        self.external_scan_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), stamp);
    }

    /// Autosave fires every couple of seconds while typing; a full snapshot per
    /// save is pure write amplification. Keep the most recent pre-edit state but
    /// rate-limit how often a new one is cut.
    fn backup_current_throttled(&self, snapshot: &NoteSnapshot) -> AppResult<()> {
        let now = Instant::now();
        {
            let mut clock = self
                .backup_clock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(last) = clock.get(&snapshot.id) {
                if now.duration_since(*last) < BACKUP_MIN_INTERVAL {
                    return Ok(());
                }
            }
            clock.insert(snapshot.id.clone(), now);
        }
        self.backup_current(snapshot)
    }

    fn backup_current(&self, snapshot: &NoteSnapshot) -> AppResult<()> {
        let backup_dir = self.backups_dir().join(&snapshot.id);
        fs::create_dir_all(&backup_dir).map_err(|error| AppError::io("创建备份目录", error))?;
        let prefix = format!(
            "{}-r{}",
            Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
            snapshot.revision
        );
        atomic_write(
            &backup_dir.join(format!("{prefix}.md")),
            snapshot.markdown.as_bytes(),
        )?;
        atomic_write_json(&backup_dir.join(format!("{prefix}.json")), &snapshot.meta)?;
        let mut markdown_backups = fs::read_dir(&backup_dir)
            .map_err(|error| AppError::io("读取备份目录", error))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(OsStr::to_str) == Some("md"))
            .collect::<Vec<_>>();
        markdown_backups.sort();
        let remove_count = markdown_backups.len().saturating_sub(BACKUP_LIMIT);
        for path in markdown_backups.into_iter().take(remove_count) {
            let json_path = path.with_extension("json");
            remove_file_if_exists(&path)?;
            remove_file_if_exists(&json_path)?;
        }
        Ok(())
    }

    fn write_conflict_copy(&self, id: &str, markdown: &str) -> AppResult<PathBuf> {
        let directory = self
            .workspace_path()
            .join(INTERNAL_DATA_DIR)
            .join("conflicts")
            .join(id);
        fs::create_dir_all(&directory).map_err(|error| AppError::io("创建冲突副本目录", error))?;
        let path = directory.join(format!(
            "{}-{}.md",
            Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
            Uuid::new_v4()
        ));
        atomic_write(&path, markdown.as_bytes())?;
        Ok(path)
    }

    fn safe_asset_path(&self, note_id: &str, relative_path: &str) -> AppResult<PathBuf> {
        validate_note_id(note_id)?;
        let components = Path::new(relative_path).components().collect::<Vec<_>>();
        if components.len() != 2
            || components[0] != Component::Normal(OsStr::new("assets"))
            || !matches!(components[1], Component::Normal(_))
        {
            return Err(AppError::new(
                "path_outside_assets",
                "图片路径必须位于当前便签 assets 目录",
            ));
        }
        let file_name = match components[1] {
            Component::Normal(name) => name,
            _ => unreachable!(),
        };
        let assets_dir = self.note_dir(note_id)?.join("assets");
        let path = assets_dir.join(file_name);
        if path.exists() {
            let canonical_assets = fs::canonicalize(&assets_dir)
                .map_err(|error| AppError::io("解析图片目录", error))?;
            let canonical_path =
                fs::canonicalize(&path).map_err(|error| AppError::io("解析图片路径", error))?;
            if !canonical_path.starts_with(&canonical_assets) {
                return Err(AppError::new(
                    "path_outside_assets",
                    "图片真实路径位于当前便签 assets 目录之外",
                ));
            }
            Ok(canonical_path)
        } else {
            Ok(path)
        }
    }

    fn save_data_storage_config(&self) -> AppResult<()> {
        atomic_write_json(
            &data_storage_config_path(&self.workspace_path()),
            &DataStorageConfig {
                schema_version: DATA_CONFIG_SCHEMA_VERSION,
                default_editor_mode: self.default_editor_mode(),
            },
        )
    }

    fn save_storage_pointer(&self) -> AppResult<()> {
        write_storage_pointer(
            &self.app_data.join(STORAGE_POINTER_FILE),
            &self.workspace_path(),
        )
    }

    fn rebuild_index(&self) -> AppResult<()> {
        self.with_index(|connection| {
            connection.execute("DELETE FROM note_search", [])?;
            Ok(())
        })?;
        for note in self.list_notes()? {
            if let Ok(snapshot) = self.read_snapshot(&note.id, true) {
                self.index_note(&note.id, &snapshot.markdown, &snapshot.meta)?;
            }
        }
        Ok(())
    }

    fn index_note(&self, id: &str, markdown: &str, meta: &NoteMeta) -> AppResult<()> {
        self.with_index(|connection| {
            // One transaction instead of two autocommits halves the WAL syncs on
            // a path that runs on every autosave.
            let transaction = connection.transaction()?;
            transaction.execute("DELETE FROM note_search WHERE id = ?1", params![id])?;
            transaction.execute(
                "INSERT INTO note_search(id, title, body, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, &meta.title, markdown, &meta.updated_at],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    /// Serializes index access and reuses one long-lived connection. Opening a
    /// connection per call meant re-running the pragmas and the
    /// `CREATE VIRTUAL TABLE` probe on every save and every external-change hit.
    fn with_index<T>(&self, action: impl FnOnce(&mut Connection) -> AppResult<T>) -> AppResult<T> {
        let mut slot = self
            .index_connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(open_index_connection(&self.index_path())?);
        }
        let connection = slot.as_mut().expect("index connection initialized");
        match action(connection) {
            Ok(value) => Ok(value),
            Err(error) => {
                // A corrupt or vanished index file must not poison every later
                // call, so drop the handle and let the next one reopen it.
                *slot = None;
                Err(error)
            }
        }
    }

    fn index_path(&self) -> PathBuf {
        self.app_data.join("search-index.sqlite3")
    }

    fn note_dir(&self, id: &str) -> AppResult<PathBuf> {
        validate_note_id(id)?;
        Ok(self.notes_dir().join(id))
    }

    fn journal_path(&self, id: &str) -> AppResult<PathBuf> {
        validate_note_id(id)?;
        Ok(self.journal_dir().join(format!("{id}.json")))
    }

    fn notes_dir(&self) -> PathBuf {
        self.workspace_path().join(INTERNAL_DATA_DIR).join("notes")
    }

    fn windows_state_path(&self) -> PathBuf {
        self.workspace_path()
            .join(INTERNAL_DATA_DIR)
            .join("state")
            .join("windows.json")
    }

    fn note_order_path(&self) -> PathBuf {
        self.workspace_path()
            .join(INTERNAL_DATA_DIR)
            .join("state")
            .join("note-order.json")
    }

    fn journal_dir(&self) -> PathBuf {
        self.workspace_path()
            .join(INTERNAL_DATA_DIR)
            .join("journal")
    }

    fn backups_dir(&self) -> PathBuf {
        self.workspace_path()
            .join(INTERNAL_DATA_DIR)
            .join("backups")
    }

    fn trash_dir(&self) -> PathBuf {
        self.workspace_path().join(INTERNAL_DATA_DIR).join("trash")
    }
}

fn resolve_workspace_configuration(
    app_data: &Path,
    legacy_app_data: &Path,
    document_root: &Path,
) -> AppResult<(PathBuf, Option<AppSettings>)> {
    let stored_settings = read_json::<AppSettings>(&app_data.join("settings.json"))
        .ok()
        .or_else(|| read_json::<AppSettings>(&legacy_app_data.join("settings.json")).ok());
    let configured_pointer = match read_storage_pointer(&app_data.join(STORAGE_POINTER_FILE))? {
        Some(path) => Some(path),
        None => read_storage_pointer(&legacy_app_data.join(STORAGE_POINTER_FILE))?,
    };
    let legacy_default_workspace = document_root.join(LEGACY_DEFAULT_WORKSPACE_DIR);
    let current_default_workspace = document_root.join(DEFAULT_WORKSPACE_DIR);
    let configured = configured_pointer
        .or_else(|| {
            stored_settings
                .as_ref()
                .map(|settings| PathBuf::from(&settings.workspace_path))
        })
        .unwrap_or_else(|| current_default_workspace.clone());
    let configured = if paths_refer_same(&configured, &legacy_default_workspace) {
        current_default_workspace
    } else {
        configured
    };
    Ok((configured, stored_settings))
}

fn legacy_workspace_has_data(path: &Path) -> bool {
    path.join(INTERNAL_DATA_DIR).is_dir()
        || path.join(LEGACY_INTERNAL_DATA_DIR).is_dir()
        || path.join("notes").is_dir()
}

fn paths_refer_same(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn legacy_note_order(notes: &[NoteSummary]) -> Vec<String> {
    let mut notes = notes.iter().collect::<Vec<_>>();
    notes.sort_by(|left, right| {
        right
            .pinned
            .cmp(&left.pinned)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    notes.into_iter().map(|note| note.id.clone()).collect()
}

fn notes_in_order(notes: Vec<NoteSummary>, ordered_ids: &[String]) -> Vec<NoteSummary> {
    let mut notes_by_id = notes
        .into_iter()
        .map(|note| (note.id.clone(), note))
        .collect::<HashMap<_, _>>();
    ordered_ids
        .iter()
        .filter_map(|id| notes_by_id.remove(id))
        .collect()
}

fn preserve_corrupt_note_order(path: &Path) {
    if !path.exists() {
        return;
    }
    let backup = path.with_file_name(format!(
        "note-order.corrupt-{}.json",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    ));
    let _ = fs::rename(path, backup);
}

fn prepare_workspace(path: &Path) -> AppResult<PathBuf> {
    let path = normalize_storage_path(path)?;
    let path = path.as_path();
    if path.as_os_str().is_empty() {
        return Err(AppError::invalid("飞花 - PetalDesk 数据存储路径不能为空"));
    }
    if !path.is_absolute() {
        return Err(AppError::invalid(
            "飞花 - PetalDesk 数据存储路径必须是绝对路径",
        ));
    }
    let internal = path.join(INTERNAL_DATA_DIR);
    for directory in [
        "notes",
        "state",
        "tools",
        "backups",
        "journal",
        "trash",
        "conflicts",
    ] {
        fs::create_dir_all(internal.join(directory))
            .map_err(|error| AppError::io("创建飞花 - PetalDesk 数据存储目录", error))?;
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| AppError::io("解析飞花 - PetalDesk 数据存储路径", error))?;
    normalize_storage_path(&canonical)
}

fn normalize_storage_path(path: &Path) -> AppResult<PathBuf> {
    let normalized = normalize_windows_display_path(path);
    #[cfg(windows)]
    if uses_unsupported_windows_namespace(&normalized) {
        return Err(AppError::invalid(
            "飞花 - PetalDesk 数据存储不支持 Windows 设备命名空间路径",
        ));
    }
    Ok(normalized)
}

/// Windows returns extended-length paths (for example `\\?\C:\notes`) from
/// `canonicalize`. They are useful for Win32 I/O, but are implementation
/// details and should never be persisted or shown in the UI.
#[cfg(windows)]
fn normalize_windows_display_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const SEPARATOR: u16 = b'\\' as u16;
    const EXTENDED_PREFIX: &[u16] = &[SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const UNC_PREFIX: &[u16] = &[
        SEPARATOR,
        SEPARATOR,
        b'?' as u16,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];

    fn starts_with_ascii_case_insensitive(value: &[u16], prefix: &[u16]) -> bool {
        fn ascii_lowercase(unit: u16) -> u16 {
            if (b'A' as u16..=b'Z' as u16).contains(&unit) {
                unit + (b'a' - b'A') as u16
            } else {
                unit
            }
        }

        value.len() >= prefix.len()
            && value
                .iter()
                .zip(prefix)
                .all(|(left, right)| ascii_lowercase(*left) == ascii_lowercase(*right))
    }

    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if starts_with_ascii_case_insensitive(&encoded, UNC_PREFIX) {
        let mut normalized = vec![SEPARATOR, SEPARATOR];
        normalized.extend_from_slice(&encoded[UNC_PREFIX.len()..]);
        return PathBuf::from(OsString::from_wide(&normalized));
    }

    if encoded.starts_with(EXTENDED_PREFIX) {
        let local = &encoded[EXTENDED_PREFIX.len()..];
        let is_drive_path = local.len() >= 3
            && ((b'A' as u16..=b'Z' as u16).contains(&local[0])
                || (b'a' as u16..=b'z' as u16).contains(&local[0]))
            && local[1] == b':' as u16
            && matches!(local[2], value if value == b'\\' as u16 || value == b'/' as u16);
        if is_drive_path {
            return PathBuf::from(OsString::from_wide(local));
        }
    }

    path.to_path_buf()
}

#[cfg(windows)]
fn uses_unsupported_windows_namespace(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;

    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let extended = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    let device = [b'\\' as u16, b'\\' as u16, b'.' as u16, b'\\' as u16];
    encoded.starts_with(&extended) || encoded.starts_with(&device)
}

#[cfg(not(windows))]
fn normalize_windows_display_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn data_storage_config_path(root: &Path) -> PathBuf {
    root.join(INTERNAL_DATA_DIR).join("config.json")
}

fn load_data_storage_config(
    root: &Path,
    legacy_settings: Option<&AppSettings>,
) -> AppResult<String> {
    let path = data_storage_config_path(root);
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<DataStorageConfig>(&bytes) {
            Ok(config) => Ok(normalize_stored_editor_mode(&config.default_editor_mode)),
            Err(_) => {
                preserve_corrupt_config(&path);
                Ok(legacy_settings
                    .map(|settings| normalize_stored_editor_mode(&settings.default_editor_mode))
                    .unwrap_or_else(|| DEFAULT_EDITOR_MODE.to_string()))
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(legacy_settings
            .map(|settings| normalize_stored_editor_mode(&settings.default_editor_mode))
            .unwrap_or_else(|| DEFAULT_EDITOR_MODE.to_string())),
        Err(error) => Err(AppError::io("读取飞花 - PetalDesk 数据配置", error)),
    }
}

fn preserve_corrupt_config(path: &Path) {
    if !path.exists() {
        return;
    }
    let backup = path.with_file_name(format!(
        "config.corrupt-{}.json",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    ));
    let _ = fs::rename(path, backup);
}

fn migrate_legacy_storage(
    root: &Path,
    legacy_workspace: Option<&Path>,
    app_data: &Path,
    legacy_app_data: Option<&Path>,
    legacy_settings: Option<&AppSettings>,
) -> AppResult<()> {
    let internal = root.join(INTERNAL_DATA_DIR);
    let marker = internal.join(LEGACY_MIGRATION_MARKER_FILE);
    if !marker.is_file() {
        if let Some(source) = legacy_workspace.filter(|source| !paths_refer_same(source, root)) {
            copy_directory_entries_if_missing(&source.join(INTERNAL_DATA_DIR), &internal)?;
            copy_directory_entries_if_missing(&source.join(LEGACY_INTERNAL_DATA_DIR), &internal)?;
            copy_directory_entries_if_missing(&source.join("notes"), &internal.join("notes"))?;
        }
        copy_directory_entries_if_missing(&root.join(LEGACY_INTERNAL_DATA_DIR), &internal)?;
        copy_directory_entries_if_missing(&root.join("notes"), &internal.join("notes"))?;

        for source_root in std::iter::once(app_data).chain(legacy_app_data) {
            for (source_name, destination) in [
                ("windows.json", internal.join("state").join("windows.json")),
                (
                    "reminders.json",
                    internal.join("tools").join("reminders.json"),
                ),
                ("gantt.json", internal.join("tools").join("gantt.json")),
                ("timer.json", internal.join("tools").join("timer.json")),
            ] {
                copy_file_if_missing(&source_root.join(source_name), &destination)?;
            }
        }
    }

    let config_path = data_storage_config_path(root);
    if !config_path.exists() {
        let default_editor_mode = legacy_settings
            .map(|settings| normalize_stored_editor_mode(&settings.default_editor_mode))
            .unwrap_or_else(|| DEFAULT_EDITOR_MODE.to_string());
        atomic_write_json(
            &config_path,
            &DataStorageConfig {
                schema_version: DATA_CONFIG_SCHEMA_VERSION,
                default_editor_mode,
            },
        )?;
    }
    if !marker.is_file() {
        atomic_write(&marker, b"1\n")?;
    }
    Ok(())
}

fn copy_directory_entries_if_missing(source: &Path, destination: &Path) -> AppResult<()> {
    if !source.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(destination).map_err(|error| AppError::io("创建迁移目标目录", error))?;
    for entry in fs::read_dir(source).map_err(|error| AppError::io("读取旧数据目录", error))?
    {
        let entry = entry.map_err(|error| AppError::io("读取旧数据条目", error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| AppError::io("读取旧数据类型", error))?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            copy_directory_entries_if_missing(&entry.path(), &target)?;
        } else if file_type.is_file() {
            copy_file_if_missing(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_file_if_missing(source: &Path, destination: &Path) -> AppResult<()> {
    if !source.is_file() || destination.exists() {
        return Ok(());
    }
    let bytes = fs::read(source).map_err(|error| AppError::io("读取旧数据文件", error))?;
    atomic_write(destination, &bytes)
}

fn read_storage_pointer(path: &Path) -> AppResult<Option<PathBuf>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("读取飞花 - PetalDesk 数据存储路径", error)),
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let decoded = if bytes.starts_with(&[0xff, 0xfe]) {
        decode_utf16_pointer(&bytes[2..], true)?
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        decode_utf16_pointer(&bytes[2..], false)?
    } else {
        String::from_utf8(bytes)
            .map_err(|_| AppError::invalid("飞花 - PetalDesk 数据存储路径文件编码无效"))?
    };
    let decoded = decoded.trim_matches(['\u{feff}', '\0', '\r', '\n', ' ']);
    if decoded.is_empty() {
        Ok(None)
    } else {
        Ok(Some(normalize_storage_path(Path::new(decoded))?))
    }
}

fn decode_utf16_pointer(bytes: &[u8], little_endian: bool) -> AppResult<String> {
    if bytes.len() % 2 != 0 {
        return Err(AppError::invalid(
            "飞花 - PetalDesk 数据存储路径文件编码无效",
        ));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect::<Vec<_>>();
    String::from_utf16(&units)
        .map_err(|_| AppError::invalid("飞花 - PetalDesk 数据存储路径文件编码无效"))
}

fn write_storage_pointer(path: &Path, root: &Path) -> AppResult<()> {
    let normalized = normalize_storage_path(root)?;
    let value = normalized.to_string_lossy();
    if value.trim().is_empty() {
        return Err(AppError::invalid("飞花 - PetalDesk 数据存储路径不能为空"));
    }
    let mut bytes = vec![0xff, 0xfe];
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    atomic_write(path, &bytes)
}

pub(crate) fn validate_note_id(id: &str) -> AppResult<()> {
    let parsed = Uuid::parse_str(id).map_err(|_| AppError::invalid("便签 ID 无效"))?;
    if parsed.to_string() != id.to_ascii_lowercase() {
        return Err(AppError::invalid("便签 ID 格式无效"));
    }
    Ok(())
}

fn validate_window_label(label: &str) -> AppResult<()> {
    if matches!(label, "main" | "timer" | "reminder" | "gantt") {
        return Ok(());
    }
    if let Some(id) = label.strip_prefix("note-") {
        return validate_note_id(id);
    }
    Err(AppError::invalid("窗口标签无效"))
}

fn validate_color_patch(color: Option<&str>) -> AppResult<()> {
    if let Some(color) = color {
        if !ALLOWED_COLORS.contains(&color) {
            return Err(AppError::invalid("不支持的便签颜色"));
        }
    }
    Ok(())
}

/// Select a palette entry from the random entropy already generated for a note id.
/// Keeping this mapping pure makes the palette behavior straightforward to test.
fn note_color_for_entropy(entropy: u128) -> &'static str {
    ALLOWED_COLORS[(entropy % ALLOWED_COLORS.len() as u128) as usize]
}

fn validate_editor_mode_patch(editor_mode: Option<&str>) -> AppResult<()> {
    if let Some(editor_mode) = editor_mode {
        if !ALLOWED_EDITOR_MODES.contains(&editor_mode) {
            return Err(AppError::invalid("不支持的编辑样式"));
        }
    }
    Ok(())
}

fn normalize_stored_editor_mode(editor_mode: &str) -> String {
    match editor_mode {
        "plain" => "plain".to_string(),
        _ => DEFAULT_EDITOR_MODE.to_string(),
    }
}

fn normalize_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        DEFAULT_NOTE_TITLE.to_string()
    } else {
        title.chars().take(200).collect()
    }
}

fn migrate_note_meta(meta: &mut NoteMeta, markdown: &str) -> bool {
    let mut changed = false;
    if meta.title.trim().is_empty() {
        meta.title = title_from_markdown(markdown);
        changed = true;
    }
    let editor_mode = normalize_stored_editor_mode(&meta.editor_mode);
    if meta.editor_mode != editor_mode {
        meta.editor_mode = editor_mode;
        changed = true;
    }
    if meta.schema_version < SCHEMA_VERSION {
        meta.schema_version = SCHEMA_VERSION;
        changed = true;
    }
    changed
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn title_from_markdown(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|line| {
            !(line.is_empty()
                || line.starts_with("![") && line.contains("](") && line.ends_with(')'))
        })
        .map(|line| {
            line.trim_start_matches('#')
                .trim_start_matches(|character: char| {
                    character == '-'
                        || character == '*'
                        || character == '+'
                        || character.is_whitespace()
                })
                .trim_start_matches("[ ]")
                .trim_start_matches("[x]")
                .trim()
                .chars()
                .take(80)
                .collect::<String>()
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "无标题便签".to_string())
}

fn excerpt_from_markdown(markdown: &str) -> String {
    strip_highlight_markers(&markdown_to_plain_text(markdown))
        .chars()
        .take(160)
        .collect()
}

fn strip_highlight_markers(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(open) = remaining.find("==") {
        let after_open = &remaining[open + 2..];
        let Some(close) = after_open.find("==") else {
            break;
        };
        let content = &after_open[..close];
        if content.is_empty()
            || content.starts_with(char::is_whitespace)
            || content.ends_with(char::is_whitespace)
        {
            output.push_str(&remaining[..open + 2]);
            remaining = after_open;
            continue;
        }
        output.push_str(&remaining[..open]);
        output.push_str(content);
        remaining = &after_open[close + 2..];
    }
    output.push_str(remaining);
    output
}

fn excerpt_from_plain_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect()
}

fn markdown_to_plain_text(markdown: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut plain = String::new();
    let mut image_depth = 0_u32;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(Tag::Image { .. }) => {
                image_depth = image_depth.saturating_add(1);
                push_plain_boundary(&mut plain);
                plain.push_str("[图片]");
                push_plain_boundary(&mut plain);
            }
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            Event::Text(text) | Event::Code(text) if image_depth == 0 => plain.push_str(&text),
            Event::Html(text) | Event::InlineHtml(text) if image_depth == 0 => {
                plain.push_str(&text)
            }
            Event::SoftBreak | Event::HardBreak | Event::Rule => push_plain_boundary(&mut plain),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::Item,
            ) => push_plain_boundary(&mut plain),
            _ => {}
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_plain_boundary(target: &mut String) {
    if !target.is_empty() && !target.ends_with(char::is_whitespace) {
        target.push(' ');
    }
}

fn summary_from_snapshot(snapshot: &NoteSnapshot) -> NoteSummary {
    NoteSummary {
        id: snapshot.id.clone(),
        title: snapshot.meta.title.clone(),
        excerpt: if snapshot.meta.editor_mode == "plain" {
            excerpt_from_plain_text(&snapshot.markdown)
        } else {
            excerpt_from_markdown(&snapshot.markdown)
        },
        editor_mode: snapshot.meta.editor_mode.clone(),
        color: snapshot.meta.color.clone(),
        pinned: snapshot.meta.pinned,
        read_only: snapshot.meta.read_only,
        created_at: snapshot.meta.created_at.clone(),
        updated_at: snapshot.meta.updated_at.clone(),
        schema_version: snapshot.meta.schema_version,
        revision: snapshot.revision,
    }
}

fn open_index_connection(path: &Path) -> AppResult<Connection> {
    let connection = match Connection::open(path) {
        Ok(connection) => connection,
        Err(_) => {
            let _ = fs::remove_file(path);
            Connection::open(path)?
        }
    };
    connection.busy_timeout(Duration::from_secs(2))?;
    if connection.execute_batch(INDEX_SCHEMA_SQL).is_err() {
        drop(connection);
        remove_sqlite_files(path);
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.execute_batch(INDEX_SCHEMA_SQL)?;
        return Ok(connection);
    }
    Ok(connection)
}

const INDEX_SCHEMA_SQL: &str = "PRAGMA journal_mode=WAL;\
     PRAGMA synchronous=NORMAL;\
     CREATE VIRTUAL TABLE IF NOT EXISTS note_search USING fts5(\
       id UNINDEXED, title, body, updated_at UNINDEXED, tokenize='unicode61'\
     );";

fn read_json<T: DeserializeOwned>(path: &Path) -> AppResult<T> {
    let bytes = fs::read(path).map_err(|error| AppError::io("读取 JSON 文件", error))?;
    serde_json::from_slice(&bytes).map_err(AppError::from)
}

pub(crate) fn atomic_write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write(path, &bytes)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::invalid("保存路径没有父目录"))?;
    fs::create_dir_all(parent).map_err(|error| AppError::io("创建保存目录", error))?;
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| AppError::invalid("保存文件名无效"))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| AppError::io("创建临时文件", error))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(AppError::io("写入临时文件", error));
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> AppResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(AppError::io(
            "原子替换文件",
            std::io::Error::last_os_error(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> AppResult<()> {
    fs::rename(source, destination).map_err(|error| AppError::io("原子替换文件", error))
}

fn remove_file_if_exists(path: &Path) -> AppResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io("删除文件", error)),
    }
}

fn remove_sqlite_files(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(sqlite_sidecar(path, "-wal"));
    let _ = fs::remove_file(sqlite_sidecar(path, "-shm"));
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, WorkspaceStore) {
        let root = TempDir::new().unwrap();
        let store =
            WorkspaceStore::for_test(&root.path().join("workspace"), &root.path().join("app"))
                .unwrap();
        (root, store)
    }

    fn listed_note_ids(store: &WorkspaceStore) -> Vec<String> {
        store
            .list_notes()
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect()
    }

    fn set_pinned(store: &WorkspaceStore, note: &NoteSnapshot, pinned: bool) -> NoteSnapshot {
        store
            .commit_note(CommitNoteRequest {
                id: note.id.clone(),
                base_revision: note.revision,
                markdown: note.markdown.clone(),
                meta_patch: NoteMetaPatch {
                    pinned: Some(pinned),
                    ..NoteMetaPatch::default()
                },
            })
            .unwrap();
        store.get_note(&note.id).unwrap()
    }

    #[test]
    fn note_color_entropy_maps_to_every_allowed_color() {
        for (index, expected) in ALLOWED_COLORS.iter().enumerate() {
            assert_eq!(note_color_for_entropy(index as u128), *expected);
            assert_eq!(
                note_color_for_entropy((index + ALLOWED_COLORS.len()) as u128),
                *expected
            );
        }
    }

    #[test]
    fn created_note_uses_an_allowed_color() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        assert!(ALLOWED_COLORS.contains(&note.meta.color.as_str()));
    }

    #[test]
    fn stores_notes_and_window_state_under_the_internal_directory() {
        let (root, store) = store();
        let note = store.create_note().unwrap();
        store
            .save_window_state(
                "main",
                WindowState {
                    x: 10.0,
                    y: 20.0,
                    width: 800.0,
                    height: 600.0,
                    maximized: false,
                },
            )
            .unwrap();

        let workspace = root.path().join("workspace");
        assert!(workspace
            .join(INTERNAL_DATA_DIR)
            .join("notes")
            .join(note.id)
            .join("note.md")
            .is_file());
        assert!(workspace
            .join(INTERNAL_DATA_DIR)
            .join("state")
            .join("windows.json")
            .is_file());
        assert!(!workspace.join("notes").exists());
        assert!(!root.path().join("app").join("windows.json").exists());
    }

    #[test]
    fn migrates_legacy_layout_without_overwriting_or_deleting_sources() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("app");
        let note_id = Uuid::new_v4().to_string();
        let legacy_note = workspace.join("notes").join(&note_id);
        fs::create_dir_all(legacy_note.join("assets")).unwrap();
        fs::write(legacy_note.join("note.md"), "旧便签").unwrap();
        fs::write(legacy_note.join("meta.json"), "{}").unwrap();
        fs::create_dir_all(&app_data).unwrap();
        fs::write(app_data.join("windows.json"), "{\"windows\":{}}").unwrap();
        fs::write(app_data.join("reminders.json"), "[]").unwrap();
        fs::write(app_data.join("gantt.json"), "[]").unwrap();
        let legacy_settings = AppSettings {
            workspace_path: workspace.to_string_lossy().into_owned(),
            default_editor_mode: "plain".to_string(),
        };

        let prepared = prepare_workspace(&workspace).unwrap();
        migrate_legacy_storage(&prepared, None, &app_data, None, Some(&legacy_settings)).unwrap();
        let migrated_note = prepared
            .join(INTERNAL_DATA_DIR)
            .join("notes")
            .join(&note_id)
            .join("note.md");
        assert_eq!(fs::read_to_string(&migrated_note).unwrap(), "旧便签");
        assert_eq!(
            fs::read_to_string(legacy_note.join("note.md")).unwrap(),
            "旧便签"
        );
        assert!(prepared
            .join(INTERNAL_DATA_DIR)
            .join("state")
            .join("windows.json")
            .is_file());
        assert!(prepared
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("reminders.json")
            .is_file());
        assert!(prepared
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("gantt.json")
            .is_file());
        let config: DataStorageConfig = read_json(&data_storage_config_path(&prepared)).unwrap();
        assert_eq!(config.default_editor_mode, "plain");

        fs::write(&migrated_note, "新位置内容").unwrap();
        migrate_legacy_storage(&prepared, None, &app_data, None, Some(&legacy_settings)).unwrap();
        assert_eq!(fs::read_to_string(&migrated_note).unwrap(), "新位置内容");
        fs::remove_file(&migrated_note).unwrap();
        migrate_legacy_storage(&prepared, None, &app_data, None, Some(&legacy_settings)).unwrap();
        assert!(!migrated_note.exists());
        assert_eq!(
            fs::read_to_string(legacy_note.join("note.md")).unwrap(),
            "旧便签"
        );
    }

    #[test]
    fn migrates_previous_internal_directory_and_local_app_data() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let app_data = root.path().join("current-app");
        let legacy_app_data = root.path().join("previous-app");
        let prepared = prepare_workspace(&workspace).unwrap();
        let legacy_internal = prepared.join(LEGACY_INTERNAL_DATA_DIR);
        fs::create_dir_all(legacy_internal.join("tools")).unwrap();
        fs::create_dir_all(legacy_internal.join("state")).unwrap();
        fs::write(
            legacy_internal.join("tools").join("timer.json"),
            "legacy timer",
        )
        .unwrap();
        fs::write(
            legacy_internal.join("tools").join("gantt.json"),
            "legacy gantt",
        )
        .unwrap();
        fs::write(
            legacy_internal.join("state").join("note-order.json"),
            "legacy order",
        )
        .unwrap();
        fs::create_dir_all(&legacy_app_data).unwrap();
        fs::write(legacy_app_data.join("reminders.json"), "legacy reminders").unwrap();
        fs::create_dir_all(prepared.join(INTERNAL_DATA_DIR).join("tools")).unwrap();
        fs::write(
            prepared
                .join(INTERNAL_DATA_DIR)
                .join("tools")
                .join("gantt.json"),
            "current gantt",
        )
        .unwrap();

        migrate_legacy_storage(&prepared, None, &app_data, Some(&legacy_app_data), None).unwrap();

        let current_internal = prepared.join(INTERNAL_DATA_DIR);
        assert_eq!(
            fs::read_to_string(current_internal.join("tools").join("timer.json")).unwrap(),
            "legacy timer"
        );
        assert_eq!(
            fs::read_to_string(current_internal.join("tools").join("gantt.json")).unwrap(),
            "current gantt"
        );
        assert_eq!(
            fs::read_to_string(current_internal.join("state").join("note-order.json")).unwrap(),
            "legacy order"
        );
        assert_eq!(
            fs::read_to_string(current_internal.join("tools").join("reminders.json")).unwrap(),
            "legacy reminders"
        );
        assert!(legacy_internal.join("tools").join("timer.json").is_file());
        assert!(legacy_app_data.join("reminders.json").is_file());
    }

    #[test]
    fn reads_utf8_and_installer_utf16_storage_pointers() {
        let root = TempDir::new().unwrap();
        let pointer = root.path().join(STORAGE_POINTER_FILE);
        let expected = PathBuf::from(format!(r"D:\资料\{} 数据", LEGACY_DEFAULT_WORKSPACE_DIR));

        fs::write(&pointer, expected.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(
            read_storage_pointer(&pointer).unwrap(),
            Some(expected.clone())
        );

        let mut utf16 = vec![0xff, 0xfe];
        for unit in format!("{}\r\n", expected.to_string_lossy()).encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&pointer, utf16).unwrap();
        assert_eq!(read_storage_pointer(&pointer).unwrap(), Some(expected));
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_extended_windows_paths_for_display_and_storage() {
        let root = TempDir::new().unwrap();
        let pointer = root.path().join(STORAGE_POINTER_FILE);
        let extended_drive = PathBuf::from(r"\\?\D:\StarsLiao\I_am\PetalDesk");
        let display_drive = PathBuf::from(r"D:\StarsLiao\I_am\PetalDesk");

        fs::write(&pointer, extended_drive.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(
            read_storage_pointer(&pointer).unwrap(),
            Some(display_drive.clone())
        );

        let mut utf16 = vec![0xff, 0xfe];
        for unit in extended_drive.to_string_lossy().encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&pointer, utf16).unwrap();
        assert_eq!(read_storage_pointer(&pointer).unwrap(), Some(display_drive));

        assert_eq!(
            normalize_windows_display_path(Path::new(r"\\?\UNC\server\share\PetalDesk")),
            PathBuf::from(r"\\server\share\PetalDesk")
        );
        assert_eq!(
            normalize_windows_display_path(Path::new(r"\\.\C:\PetalDesk")),
            PathBuf::from(r"\\.\C:\PetalDesk")
        );
        assert!(normalize_storage_path(Path::new(r"\\.\C:\PetalDesk")).is_err());
        assert!(normalize_storage_path(Path::new(
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\PetalDesk"
        ))
        .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn writing_storage_pointer_never_persists_extended_prefix() {
        let root = TempDir::new().unwrap();
        let pointer = root.path().join(STORAGE_POINTER_FILE);

        write_storage_pointer(&pointer, Path::new(r"\\?\D:\StarsLiao\I_am\PetalDesk")).unwrap();

        let bytes = fs::read(&pointer).unwrap();
        assert!(bytes.starts_with(&[0xff, 0xfe]));
        let decoded = decode_utf16_pointer(&bytes[2..], true).unwrap();
        assert_eq!(decoded, r"D:\StarsLiao\I_am\PetalDesk");
        assert!(!decoded.starts_with(r"\\?\"));
    }

    #[test]
    fn workspace_resolution_prefers_current_configuration_and_reads_previous_locations() {
        let root = TempDir::new().unwrap();
        let app_data = root.path().join("current-app");
        let legacy_app_data = root.path().join("previous-app");
        let documents = root.path().join("documents");
        let current_workspace = root.path().join("current-workspace");
        let legacy_workspace = root.path().join("previous-workspace");
        for path in [
            &app_data,
            &legacy_app_data,
            &documents,
            &current_workspace,
            &legacy_workspace,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        write_storage_pointer(
            &legacy_app_data.join(STORAGE_POINTER_FILE),
            &legacy_workspace,
        )
        .unwrap();

        let (resolved, _) =
            resolve_workspace_configuration(&app_data, &legacy_app_data, &documents).unwrap();
        assert_eq!(resolved, legacy_workspace);

        write_storage_pointer(&app_data.join(STORAGE_POINTER_FILE), &current_workspace).unwrap();
        let (resolved, _) =
            resolve_workspace_configuration(&app_data, &legacy_app_data, &documents).unwrap();
        assert_eq!(resolved, current_workspace);

        fs::remove_file(app_data.join(STORAGE_POINTER_FILE)).unwrap();
        fs::remove_file(legacy_app_data.join(STORAGE_POINTER_FILE)).unwrap();
        let settings_workspace = root.path().join("settings-workspace");
        atomic_write_json(
            &legacy_app_data.join("settings.json"),
            &AppSettings {
                workspace_path: settings_workspace.to_string_lossy().into_owned(),
                default_editor_mode: "plain".to_string(),
            },
        )
        .unwrap();
        let (resolved, settings) =
            resolve_workspace_configuration(&app_data, &legacy_app_data, &documents).unwrap();
        assert_eq!(resolved, settings_workspace);
        assert_eq!(settings.unwrap().default_editor_mode, "plain");

        fs::remove_file(legacy_app_data.join("settings.json")).unwrap();
        let legacy_default = documents.join(LEGACY_DEFAULT_WORKSPACE_DIR);
        fs::create_dir_all(legacy_default.join(LEGACY_INTERNAL_DATA_DIR)).unwrap();
        let (resolved, _) =
            resolve_workspace_configuration(&app_data, &legacy_app_data, &documents).unwrap();
        assert_eq!(resolved, documents.join(DEFAULT_WORKSPACE_DIR));
    }

    #[test]
    fn migrates_the_previous_default_workspace_into_petaldesk() {
        let root = TempDir::new().unwrap();
        let documents = root.path().join("documents");
        let legacy_workspace = documents.join(LEGACY_DEFAULT_WORKSPACE_DIR);
        let current_workspace = documents.join(DEFAULT_WORKSPACE_DIR);
        let app_data = root.path().join("current-app");
        let legacy_app_data = root.path().join("previous-app");
        let note_id = Uuid::new_v4().to_string();

        fs::create_dir_all(
            legacy_workspace
                .join(LEGACY_INTERNAL_DATA_DIR)
                .join("notes")
                .join(&note_id),
        )
        .unwrap();
        fs::write(
            legacy_workspace
                .join(LEGACY_INTERNAL_DATA_DIR)
                .join("notes")
                .join(&note_id)
                .join("note.md"),
            "previous note",
        )
        .unwrap();
        fs::create_dir_all(&app_data).unwrap();
        fs::create_dir_all(&legacy_app_data).unwrap();

        let prepared = prepare_workspace(&current_workspace).unwrap();
        migrate_legacy_storage(
            &prepared,
            Some(&legacy_workspace),
            &app_data,
            Some(&legacy_app_data),
            None,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(
                prepared
                    .join(INTERNAL_DATA_DIR)
                    .join("notes")
                    .join(note_id)
                    .join("note.md")
            )
            .unwrap(),
            "previous note"
        );
        assert!(prepared
            .join(INTERNAL_DATA_DIR)
            .join(LEGACY_MIGRATION_MARKER_FILE)
            .is_file());
        assert!(legacy_workspace.join(LEGACY_INTERNAL_DATA_DIR).is_dir());
    }

    #[test]
    fn changing_data_storage_path_updates_only_the_restart_pointer() {
        let (root, store) = store();
        let original = store.workspace_path();
        let target = root.path().join("新的 PetalDesk 数据");

        let result = store
            .set_data_storage_path(target.to_string_lossy().into_owned())
            .unwrap();

        assert!(result.restart_required);
        assert_eq!(store.workspace_path(), original);
        let prepared_target = normalize_windows_display_path(&fs::canonicalize(target).unwrap());
        assert_eq!(PathBuf::from(result.path), prepared_target);
        assert_eq!(
            read_storage_pointer(&root.path().join("app").join(STORAGE_POINTER_FILE)).unwrap(),
            Some(prepared_target.clone())
        );
        assert!(fs::read(root.path().join("app").join(STORAGE_POINTER_FILE))
            .unwrap()
            .starts_with(&[0xff, 0xfe]));
        assert!(prepared_target
            .join(INTERNAL_DATA_DIR)
            .join("notes")
            .is_dir());
        let config: DataStorageConfig =
            read_json(&data_storage_config_path(&prepared_target)).unwrap();
        assert_eq!(config.default_editor_mode, DEFAULT_EDITOR_MODE);
    }

    #[test]
    fn creates_commits_and_lists_notes() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        let result = store
            .commit_note(CommitNoteRequest {
                id: note.id.clone(),
                base_revision: note.revision,
                markdown: "# 第一条\n\n正文".to_string(),
                meta_patch: NoteMetaPatch {
                    title: Some("独立标题".to_string()),
                    editor_mode: Some("plain".to_string()),
                    color: Some("blue".to_string()),
                    pinned: Some(true),
                    read_only: Some(true),
                },
            })
            .unwrap();
        assert_eq!(result.revision, 1);
        let notes = store.list_notes().unwrap();
        assert_eq!(notes[0].title, "独立标题");
        assert_eq!(notes[0].editor_mode, "plain");
        assert_eq!(notes[0].color, "blue");
        assert!(notes[0].pinned);
        assert!(notes[0].read_only);
        assert!(store.get_note(&note.id).unwrap().meta.read_only);
        let persisted: NoteMeta =
            read_json(&store.note_dir(&note.id).unwrap().join("meta.json")).unwrap();
        assert!(persisted.read_only);
    }

    #[test]
    fn new_notes_append_and_content_updates_keep_manual_order() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        store
            .reorder_notes(vec![second.id.clone(), first.id.clone()])
            .unwrap();

        let third = store.create_note().unwrap();
        assert_eq!(
            listed_note_ids(&store),
            vec![second.id.clone(), first.id.clone(), third.id.clone()]
        );

        store
            .commit_note(CommitNoteRequest {
                id: first.id.clone(),
                base_revision: first.revision,
                markdown: "刚刚更新的正文".to_string(),
                meta_patch: NoteMetaPatch {
                    title: Some("刚刚更新".to_string()),
                    ..NoteMetaPatch::default()
                },
            })
            .unwrap();
        assert_eq!(listed_note_ids(&store), vec![second.id, first.id, third.id]);
    }

    #[test]
    fn manual_note_order_persists_across_store_reloads() {
        let (root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        let third = store.create_note().unwrap();
        let expected = vec![third.id.clone(), first.id.clone(), second.id.clone()];
        store.reorder_notes(expected.clone()).unwrap();
        drop(store);

        let reloaded =
            WorkspaceStore::for_test(&root.path().join("workspace"), &root.path().join("app"))
                .unwrap();
        assert_eq!(listed_note_ids(&reloaded), expected);
    }

    #[test]
    fn pinning_moves_a_note_to_front_but_unpinning_keeps_its_position() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        let third = store.create_note().unwrap();
        store
            .reorder_notes(vec![second.id.clone(), third.id.clone(), first.id.clone()])
            .unwrap();

        let pinned = set_pinned(&store, &first, true);
        assert_eq!(
            listed_note_ids(&store),
            vec![first.id.clone(), second.id.clone(), third.id.clone()]
        );

        set_pinned(&store, &pinned, false);
        assert_eq!(listed_note_ids(&store), vec![first.id, second.id, third.id]);
    }

    #[test]
    fn deleting_removes_an_order_entry_and_restoring_appends_it() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        let third = store.create_note().unwrap();
        store
            .reorder_notes(vec![second.id.clone(), first.id.clone(), third.id.clone()])
            .unwrap();

        store.delete_note(&first.id).unwrap();
        assert_eq!(
            listed_note_ids(&store),
            vec![second.id.clone(), third.id.clone()]
        );
        store.restore_note(&first.id).unwrap();
        assert_eq!(listed_note_ids(&store), vec![second.id, third.id, first.id]);
    }

    #[test]
    fn rejects_incomplete_duplicate_and_unknown_note_orders() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        let expected = vec![first.id.clone(), second.id.clone()];

        let incomplete = store.reorder_notes(vec![first.id.clone()]).unwrap_err();
        assert_eq!(incomplete.code, "invalid_input");
        let duplicate = store
            .reorder_notes(vec![first.id.clone(), first.id.clone()])
            .unwrap_err();
        assert_eq!(duplicate.code, "invalid_input");
        let unknown = store
            .reorder_notes(vec![first.id.clone(), Uuid::new_v4().to_string()])
            .unwrap_err();
        assert_eq!(unknown.code, "invalid_input");
        assert_eq!(listed_note_ids(&store), expected);
    }

    #[test]
    fn unreadable_note_keeps_its_order_slot_and_blocks_reordering() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        store
            .reorder_notes(vec![second.id.clone(), first.id.clone()])
            .unwrap();
        let order_before = fs::read(store.note_order_path()).unwrap();
        let meta_path = store.note_dir(&second.id).unwrap().join("meta.json");
        let unavailable_path = meta_path.with_extension("unavailable");
        fs::rename(&meta_path, &unavailable_path).unwrap();

        assert_eq!(listed_note_ids(&store), vec![first.id.clone()]);
        let error = store.reorder_notes(vec![first.id.clone()]).unwrap_err();
        assert_eq!(error.code, "notes_temporarily_unavailable");
        assert_eq!(fs::read(store.note_order_path()).unwrap(), order_before);

        fs::rename(&unavailable_path, &meta_path).unwrap();
        assert_eq!(listed_note_ids(&store), vec![second.id, first.id]);
    }

    #[test]
    fn failed_pin_commit_rolls_back_the_prepared_order() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        let blocked_backup_path = store.backups_dir().join(&second.id);
        fs::write(&blocked_backup_path, b"not a directory").unwrap();

        let error = store
            .commit_note(CommitNoteRequest {
                id: second.id.clone(),
                base_revision: second.revision,
                markdown: second.markdown.clone(),
                meta_patch: NoteMetaPatch {
                    pinned: Some(true),
                    ..NoteMetaPatch::default()
                },
            })
            .unwrap_err();

        assert_eq!(error.code, "io_error");
        assert_eq!(listed_note_ids(&store), vec![first.id, second.id.clone()]);
        let unchanged = store.get_note(&second.id).unwrap();
        assert_eq!(unchanged.revision, second.revision);
        assert!(!unchanged.meta.pinned);
    }

    #[test]
    fn missing_order_file_is_seeded_from_the_previous_list_order() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        let third = store.create_note().unwrap();
        for (note, pinned, updated_at) in [
            (&first, true, "2026-01-01T00:00:00.000Z"),
            (&second, false, "2026-02-01T00:00:00.000Z"),
            (&third, false, "2026-03-01T00:00:00.000Z"),
        ] {
            let path = store.note_dir(&note.id).unwrap().join("meta.json");
            let mut meta = read_json::<NoteMeta>(&path).unwrap();
            meta.pinned = pinned;
            meta.updated_at = updated_at.to_string();
            atomic_write_json(&path, &meta).unwrap();
        }
        fs::remove_file(store.note_order_path()).unwrap();

        assert_eq!(
            listed_note_ids(&store),
            vec![first.id.clone(), third.id.clone(), second.id.clone()]
        );
        let persisted: StoredNoteOrder = read_json(&store.note_order_path()).unwrap();
        assert_eq!(persisted.ordered_ids, vec![first.id, third.id, second.id]);
    }

    #[test]
    fn first_note_follows_manual_order() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();
        store
            .reorder_notes(vec![second.id.clone(), first.id])
            .unwrap();

        assert_eq!(store.first_note_id().unwrap(), Some(second.id));
    }

    #[test]
    fn rejects_stale_revision_and_preserves_conflict_copy() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        store
            .commit_note(CommitNoteRequest {
                id: note.id.clone(),
                base_revision: 0,
                markdown: "current".to_string(),
                meta_patch: NoteMetaPatch::default(),
            })
            .unwrap();
        let error = store
            .commit_note(CommitNoteRequest {
                id: note.id,
                base_revision: 0,
                markdown: "incoming".to_string(),
                meta_patch: NoteMetaPatch::default(),
            })
            .unwrap_err();
        assert_eq!(error.code, "revision_conflict");
        let conflict = error.details.unwrap()["conflictPath"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(fs::read_to_string(conflict).unwrap(), "incoming");
    }

    #[test]
    fn detects_external_file_changes() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        fs::write(
            store.note_dir(&note.id).unwrap().join("note.md"),
            "external",
        )
        .unwrap();
        let changed = store.get_note(&note.id).unwrap();
        assert_eq!(changed.revision, 1);
        assert_eq!(changed.markdown, "external");
    }

    #[test]
    fn external_scan_skips_instead_of_waiting_for_an_active_mutation() {
        let (_root, store) = store();
        let _mutation = store.mutation_lock.lock().unwrap();

        assert!(store.detect_external_changes().unwrap().is_empty());
    }

    #[test]
    fn moves_notes_through_trash() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        store.delete_note(&note.id).unwrap();
        assert!(store.list_notes().unwrap().is_empty());
        assert_eq!(store.list_trash().unwrap().len(), 1);
        store.restore_note(&note.id).unwrap();
        assert_eq!(store.list_notes().unwrap().len(), 1);
    }

    #[test]
    fn blocks_asset_path_traversal() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        let error = store.read_asset(&note.id, "assets/../note.md").unwrap_err();
        assert_eq!(error.code, "path_outside_assets");
    }

    #[test]
    fn recovers_incomplete_journal() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        let mut meta = note.meta;
        meta.revision = 1;
        meta.read_only = true;
        meta.content_hash = content_hash(b"recovered");
        let journal = JournalEntry {
            note_id: note.id.clone(),
            base_revision: 0,
            new_revision: 1,
            markdown: "recovered".to_string(),
            meta,
            created_at: now(),
        };
        atomic_write_json(&store.journal_path(&note.id).unwrap(), &journal).unwrap();
        let recovered = store.recover_journals().unwrap();
        assert_eq!(recovered[0].status, "restored");
        let recovered_note = store.get_note(&note.id).unwrap();
        assert_eq!(recovered_note.markdown, "recovered");
        assert!(recovered_note.meta.read_only);
    }

    #[test]
    fn searches_indexed_markdown() {
        let (_root, store) = store();
        let note = store.create_note().unwrap();
        store
            .commit_note(CommitNoteRequest {
                id: note.id,
                base_revision: 0,
                markdown: "# Rust 便签\nTauri 本地应用".to_string(),
                meta_patch: NoteMetaPatch {
                    title: Some("Rust 便签".to_string()),
                    ..NoteMetaPatch::default()
                },
            })
            .unwrap();
        let results = store.search_notes("Tauri", None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].note.title, "Rust 便签");
    }

    #[test]
    fn skips_an_image_only_line_when_deriving_the_title() {
        let markdown = "![封面](assets/cover.png)\n\n# 正文标题";

        assert_eq!(title_from_markdown(markdown), "正文标题");
        assert!(excerpt_from_markdown(markdown).starts_with("[图片]"));
    }

    #[test]
    fn remembers_only_windows_left_open() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();

        store.set_note_window_open(&first.id, true).unwrap();
        store.set_note_window_open(&second.id, true).unwrap();
        store.set_note_window_open(&first.id, false).unwrap();

        let states = read_json::<StoredWindowStates>(&store.windows_state_path()).unwrap();
        assert_eq!(states.open_notes, vec![second.id]);
    }

    #[test]
    fn persists_compact_timer_window_state_without_weakening_other_windows() {
        let (_root, store) = store();
        let compact = WindowState {
            x: 12.0,
            y: 34.0,
            width: WorkspaceStore::TIMER_MIN_WIDTH,
            height: WorkspaceStore::TIMER_MIN_HEIGHT,
            maximized: false,
        };

        store.save_window_state("timer", compact.clone()).unwrap();
        assert_eq!(store.window_state("timer"), Some(compact.clone()));
        assert!(store.save_window_state("main", compact).is_err());
        assert!(store
            .save_window_state(
                "timer",
                WindowState {
                    x: 12.0,
                    y: 34.0,
                    width: WorkspaceStore::TIMER_MIN_WIDTH - 1.0,
                    height: WorkspaceStore::TIMER_MIN_HEIGHT,
                    maximized: false,
                }
            )
            .is_err());
        assert!(store
            .save_window_state(
                "timer",
                WindowState {
                    x: 12.0,
                    y: 34.0,
                    width: WorkspaceStore::TIMER_MIN_WIDTH,
                    height: WorkspaceStore::TIMER_MIN_HEIGHT - 1.0,
                    maximized: false,
                }
            )
            .is_err());
        assert!(store
            .save_window_state(
                "open-tool:timer",
                WindowState {
                    x: 12.0,
                    y: 34.0,
                    width: 320.0,
                    height: 140.0,
                    maximized: false,
                }
            )
            .is_err());
    }

    #[test]
    fn clamps_oversized_timer_window_state_on_save_and_restore() {
        let (_root, store) = store();
        let oversized = WindowState {
            x: 12.0,
            y: 34.0,
            width: 1_200.0,
            height: 900.0,
            maximized: true,
        };

        store.save_window_state("timer", oversized.clone()).unwrap();
        assert_eq!(
            store.window_state("timer"),
            Some(WindowState {
                x: oversized.x,
                y: oversized.y,
                width: WorkspaceStore::TIMER_MAX_WIDTH,
                height: WorkspaceStore::TIMER_MAX_HEIGHT,
                maximized: false,
            })
        );

        let path = store.windows_state_path();
        let states = StoredWindowStates {
            windows: std::collections::HashMap::from([("timer".to_string(), oversized)]),
            ..StoredWindowStates::default()
        };
        atomic_write_json(&path, &states).unwrap();
        assert_eq!(
            store.window_state("timer"),
            Some(WindowState {
                x: 12.0,
                y: 34.0,
                width: WorkspaceStore::TIMER_MAX_WIDTH,
                height: WorkspaceStore::TIMER_MAX_HEIGHT,
                maximized: false,
            })
        );
    }

    #[test]
    fn ignores_invalid_persisted_timer_window_state() {
        let (_root, store) = store();
        let path = store.windows_state_path();
        let states = StoredWindowStates {
            windows: std::collections::HashMap::from([(
                "timer".to_string(),
                WindowState {
                    x: 20.0,
                    y: 30.0,
                    width: WorkspaceStore::TIMER_MIN_WIDTH - 1.0,
                    height: WorkspaceStore::TIMER_MIN_HEIGHT,
                    maximized: false,
                },
            )]),
            ..StoredWindowStates::default()
        };
        atomic_write_json(&path, &states).unwrap();

        assert_eq!(store.window_state("timer"), None);
    }

    #[test]
    fn persists_reminder_window_state_with_its_own_minimum_size() {
        let (_root, store) = store();
        let reminder = WindowState {
            x: 48.0,
            y: 72.0,
            width: 560.0,
            height: 620.0,
            maximized: false,
        };

        store
            .save_window_state("reminder", reminder.clone())
            .unwrap();
        assert_eq!(store.window_state("reminder"), Some(reminder));
        assert!(store
            .save_window_state(
                "reminder",
                WindowState {
                    x: 0.0,
                    y: 0.0,
                    width: 439.0,
                    height: 360.0,
                    maximized: false,
                }
            )
            .is_err());
    }

    #[test]
    fn persists_gantt_window_state_with_its_own_minimum_size() {
        let (_root, store) = store();
        let gantt = WindowState {
            x: 64.0,
            y: 80.0,
            width: 980.0,
            height: 600.0,
            maximized: false,
        };

        store.save_window_state("gantt", gantt.clone()).unwrap();
        assert_eq!(store.window_state("gantt"), Some(gantt));
        assert!(store
            .save_window_state(
                "gantt",
                WindowState {
                    x: 0.0,
                    y: 0.0,
                    width: WorkspaceStore::GANTT_MIN_WIDTH - 1.0,
                    height: WorkspaceStore::GANTT_MIN_HEIGHT,
                    maximized: false,
                }
            )
            .is_err());
        assert!(store
            .save_window_state(
                "gantt",
                WindowState {
                    x: 0.0,
                    y: 0.0,
                    width: WorkspaceStore::GANTT_MIN_WIDTH,
                    height: WorkspaceStore::GANTT_MIN_HEIGHT - 1.0,
                    maximized: false,
                }
            )
            .is_err());
    }

    #[test]
    fn remembers_the_last_note_when_it_is_opened_or_closed() {
        let (_root, store) = store();
        let first = store.create_note().unwrap();
        let second = store.create_note().unwrap();

        store.set_note_window_open(&first.id, true).unwrap();
        assert_eq!(store.last_or_recent_note_id().unwrap(), Some(first.id));

        store.set_note_window_open(&second.id, true).unwrap();
        store.set_note_window_open(&second.id, false).unwrap();
        assert_eq!(
            store.last_or_recent_note_id().unwrap(),
            Some(second.id.clone())
        );

        let states = read_json::<StoredWindowStates>(&store.windows_state_path()).unwrap();
        assert_eq!(states.last_note_id, Some(second.id));
    }

    #[test]
    fn falls_back_to_the_most_recent_note_when_the_last_note_is_missing() {
        let (_root, store) = store();
        let older = store.create_note().unwrap();
        let newer = store.create_note().unwrap();
        let older_meta_path = store.note_dir(&older.id).unwrap().join("meta.json");
        let newer_meta_path = store.note_dir(&newer.id).unwrap().join("meta.json");
        let mut older_meta = read_json::<NoteMeta>(&older_meta_path).unwrap();
        let mut newer_meta = read_json::<NoteMeta>(&newer_meta_path).unwrap();
        older_meta.updated_at = "2026-01-01T00:00:00.000Z".to_string();
        newer_meta.updated_at = "2026-02-01T00:00:00.000Z".to_string();
        atomic_write_json(&older_meta_path, &older_meta).unwrap();
        atomic_write_json(&newer_meta_path, &newer_meta).unwrap();

        let states = StoredWindowStates {
            last_note_id: Some(Uuid::new_v4().to_string()),
            ..StoredWindowStates::default()
        };
        atomic_write_json(&store.windows_state_path(), &states).unwrap();

        assert_eq!(store.last_or_recent_note_id().unwrap(), Some(newer.id));
    }

    #[test]
    fn has_no_last_note_when_the_workspace_is_empty() {
        let (_root, store) = store();

        assert_eq!(store.last_or_recent_note_id().unwrap(), None);
    }

    #[test]
    fn validates_and_persists_the_default_editor_mode() {
        let (_root, store) = store();

        assert_eq!(store.default_editor_mode(), "typora");
        assert_eq!(store.set_default_editor_mode("plain").unwrap(), "plain");
        assert_eq!(store.default_editor_mode(), "plain");

        let config =
            read_json::<DataStorageConfig>(&data_storage_config_path(&store.workspace_path()))
                .unwrap();
        assert_eq!(config.schema_version, DATA_CONFIG_SCHEMA_VERSION);
        assert_eq!(config.default_editor_mode, "plain");

        let error = store.set_default_editor_mode("unknown").unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(store.default_editor_mode(), "plain");
    }

    #[test]
    fn newly_created_notes_capture_the_current_default_mode() {
        let (_root, store) = store();
        store.set_default_editor_mode("plain").unwrap();
        let plain_note = store.create_note().unwrap();
        store.set_default_editor_mode("typora").unwrap();
        let typora_note = store.create_note().unwrap();

        assert_eq!(plain_note.meta.editor_mode, "plain");
        assert_eq!(
            store.get_note(&plain_note.id).unwrap().meta.editor_mode,
            "plain"
        );
        assert_eq!(typora_note.meta.editor_mode, "typora");
    }

    #[test]
    fn migrates_legacy_note_metadata_without_touching_markdown() {
        let (_root, store) = store();
        store.set_default_editor_mode("plain").unwrap();
        let id = Uuid::new_v4().to_string();
        let note_dir = store.note_dir(&id).unwrap();
        fs::create_dir_all(note_dir.join("assets")).unwrap();
        let markdown = "![封面](assets/cover.png)\n\n# 旧便签标题\n\n**正文**";
        atomic_write(&note_dir.join("note.md"), markdown.as_bytes()).unwrap();
        atomic_write_json(
            &note_dir.join("meta.json"),
            &json!({
                "id": id,
                "color": "yellow",
                "pinned": false,
                "createdAt": now(),
                "updatedAt": now(),
                "schemaVersion": 1,
                "revision": 3,
                "contentHash": content_hash(markdown.as_bytes())
            }),
        )
        .unwrap();

        let snapshot = store.get_note(&id).unwrap();
        assert_eq!(snapshot.meta.title, "旧便签标题");
        assert_eq!(snapshot.meta.editor_mode, "typora");
        assert!(!snapshot.meta.read_only);
        assert_eq!(snapshot.meta.schema_version, SCHEMA_VERSION);
        assert_eq!(snapshot.revision, 3);
        assert_eq!(
            fs::read_to_string(note_dir.join("note.md")).unwrap(),
            markdown
        );

        let persisted: NoteMeta = read_json(&note_dir.join("meta.json")).unwrap();
        assert_eq!(persisted.title, "旧便签标题");
        assert_eq!(persisted.editor_mode, "typora");
        assert!(!persisted.read_only);
        assert_eq!(persisted.schema_version, SCHEMA_VERSION);
        let persisted_json: serde_json::Value = read_json(&note_dir.join("meta.json")).unwrap();
        assert_eq!(persisted_json["readOnly"], false);
    }

    #[test]
    fn reads_legacy_editor_mode_setting_and_normalizes_removed_modes() {
        let legacy: AppSettings = serde_json::from_value(json!({
            "workspacePath": "C:/notes",
            "editorMode": "source"
        }))
        .unwrap();

        assert_eq!(legacy.default_editor_mode, "source");
        assert_eq!(
            normalize_stored_editor_mode(&legacy.default_editor_mode),
            "typora"
        );
    }

    #[test]
    fn markdown_excerpt_contains_rendered_text_instead_of_source_markers() {
        let markdown =
            "### 标题\n\n- [x] **完成** ==高亮== [链接](https://example.com)\n- `代码`\n\n---";

        assert_eq!(excerpt_from_markdown(markdown), "标题 完成 高亮 链接 代码");
    }

    #[test]
    fn plain_text_excerpt_preserves_literal_markdown_characters() {
        assert_eq!(
            excerpt_from_plain_text("# 标题\n\n**这只是纯文本**"),
            "# 标题 **这只是纯文本**"
        );
    }
}
