use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write, ensure_managed_subdirectory, INTERNAL_DATA_DIR};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

const DATE_FORMAT: &str = "%Y-%m-%d";
const MAX_NAME_CHARS: usize = 200;
const GANTT_SCHEMA_VERSION: u32 = 1;
const MAX_BACKUPS: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GanttTask {
    pub id: String,
    pub name: String,
    pub progress: u8,
    pub start_date: String,
    #[serde(default)]
    pub start_hour: u8,
    pub end_date: String,
    #[serde(default = "default_end_hour")]
    pub end_hour: u8,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpsertGanttTaskRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub progress: u8,
    pub start_date: String,
    #[serde(default)]
    pub start_hour: u8,
    pub end_date: String,
    #[serde(default = "default_end_hour")]
    pub end_hour: u8,
}

const fn default_end_hour() -> u8 {
    23
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GanttDocument {
    schema_version: u32,
    document_id: String,
    revision: u64,
    updated_at: String,
    tasks: Vec<GanttTask>,
}

#[derive(Debug)]
struct DecodedDocument {
    document: GanttDocument,
    rewrite_required: bool,
}

#[derive(Debug)]
struct GanttState {
    document: GanttDocument,
    disk_hash: Option<[u8; 32]>,
    write_blocked: bool,
}

pub struct GanttStore {
    path: PathBuf,
    state: Mutex<GanttState>,
}

impl GanttStore {
    pub fn load(root: &Path) -> AppResult<Self> {
        let tools = ensure_managed_subdirectory(root, &[INTERNAL_DATA_DIR, "tools"])?;
        let gantt = ensure_managed_subdirectory(root, &[INTERNAL_DATA_DIR, "tools", "gantt"])?;
        for segments in [
            &[INTERNAL_DATA_DIR, "tools", "gantt", "backups"][..],
            &[INTERNAL_DATA_DIR, "tools", "gantt", "backups", "migrations"][..],
            &[INTERNAL_DATA_DIR, "tools", "gantt", "conflicts"][..],
            &[INTERNAL_DATA_DIR, "tools", "gantt", "corrupt"][..],
        ] {
            ensure_managed_subdirectory(root, segments)?;
        }
        Self::load_from(&gantt.join("gantt.json"), Some(&tools.join("gantt.json")))
    }

    fn load_from(path: &Path, legacy_path: Option<&Path>) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| AppError::io("创建甘特图数据目录", error))?;
        }

        migrate_legacy_file(path, legacy_path)?;
        let (document, disk_hash, write_blocked) = load_document(path)?;

        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(GanttState {
                document,
                disk_hash,
                write_blocked,
            }),
        })
    }

    #[cfg(test)]
    fn for_test(app_data: &Path) -> AppResult<Self> {
        Self::load_from(
            &app_data.join("gantt").join("gantt.json"),
            Some(&app_data.join("gantt.json")),
        )
    }

    pub fn list(&self) -> Vec<GanttTask> {
        self.state
            .lock()
            .expect("gantt store lock poisoned")
            .document
            .tasks
            .clone()
    }

    pub fn upsert(&self, request: UpsertGanttTaskRequest) -> AppResult<GanttTask> {
        self.upsert_at(request, now())
    }

    fn upsert_at(
        &self,
        request: UpsertGanttTaskRequest,
        updated_at: String,
    ) -> AppResult<GanttTask> {
        let (name, start_date, end_date) = validate_task_fields(
            &request.name,
            request.progress,
            &request.start_date,
            request.start_hour,
            &request.end_date,
            request.end_hour,
        )?;

        let mut state = self.state.lock().expect("gantt store lock poisoned");
        let mut document = state.document.clone();
        let task = if let Some(id) = request.id {
            validate_task_id(&id)?;
            let task = document
                .tasks
                .iter_mut()
                .find(|task| task.id == id)
                .ok_or_else(|| AppError::not_found("没有找到这个甘特图任务"))?;
            if task.name == name
                && task.progress == request.progress
                && task.start_date == start_date
                && task.start_hour == request.start_hour
                && task.end_date == end_date
                && task.end_hour == request.end_hour
            {
                return Ok(task.clone());
            }
            task.name = name;
            task.progress = request.progress;
            task.start_date = start_date;
            task.start_hour = request.start_hour;
            task.end_date = end_date;
            task.end_hour = request.end_hour;
            task.updated_at = updated_at;
            task.clone()
        } else {
            let task = GanttTask {
                id: Uuid::new_v4().to_string(),
                name,
                progress: request.progress,
                start_date,
                start_hour: request.start_hour,
                end_date,
                end_hour: request.end_hour,
                created_at: updated_at.clone(),
                updated_at,
            };
            document.tasks.push(task.clone());
            task
        };

        advance_document(&mut document, task.updated_at.clone())?;
        self.persist(&mut state, document)?;
        Ok(task)
    }

    pub fn delete(&self, task_id: &str) -> AppResult<()> {
        validate_task_id(task_id)?;
        let mut state = self.state.lock().expect("gantt store lock poisoned");
        let mut document = state.document.clone();
        let original_len = document.tasks.len();
        document.tasks.retain(|task| task.id != task_id);
        if document.tasks.len() == original_len {
            return Err(AppError::not_found("没有找到这个甘特图任务"));
        }
        advance_document(&mut document, now())?;
        self.persist(&mut state, document)?;
        Ok(())
    }

    pub fn reorder(&self, ordered_ids: Vec<String>) -> AppResult<()> {
        let mut requested_ids = HashSet::with_capacity(ordered_ids.len());
        for id in &ordered_ids {
            validate_task_id(id)?;
            if !requested_ids.insert(id.as_str()) {
                return Err(AppError::invalid("甘特图任务排序不能包含重复 ID"));
            }
        }

        let mut state = self.state.lock().expect("gantt store lock poisoned");
        if ordered_ids.len() != state.document.tasks.len() {
            return Err(AppError::invalid("甘特图任务排序必须包含全部任务 ID"));
        }

        let existing_ids = state
            .document
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect::<HashSet<_>>();
        if requested_ids != existing_ids {
            return Err(AppError::invalid("甘特图任务排序包含未知或缺失的 ID"));
        }
        if state
            .document
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .eq(ordered_ids.iter().map(String::as_str))
        {
            return Ok(());
        }

        let mut tasks_by_id = state
            .document
            .tasks
            .iter()
            .cloned()
            .map(|task| (task.id.clone(), task))
            .collect::<HashMap<_, _>>();
        let tasks = ordered_ids
            .iter()
            .map(|id| {
                tasks_by_id
                    .remove(id)
                    .expect("validated gantt task ID must exist")
            })
            .collect::<Vec<_>>();

        let mut document = state.document.clone();
        document.tasks = tasks;
        advance_document(&mut document, now())?;
        self.persist(&mut state, document)?;
        Ok(())
    }

    fn persist(&self, state: &mut GanttState, document: GanttDocument) -> AppResult<()> {
        let proposed_bytes = serde_json::to_vec_pretty(&document)?;
        if state.write_blocked {
            let (details, conflict_saved) = pending_snapshot_details(&self.path, &proposed_bytes);
            let message = if conflict_saved {
                "甘特图主文件已损坏且没有可用备份，未覆盖原文件；本次修改已保存到冲突目录"
            } else {
                "甘特图主文件已损坏且没有可用备份，未覆盖原文件；本次修改也未能保存到冲突目录"
            };
            return Err(AppError::new("gantt_corrupt", message).with_details(details));
        }

        let current_bytes = read_optional_file(&self.path, "核对甘特图数据文件")?;
        let current_hash = current_bytes.as_deref().map(hash_bytes);
        if current_hash != state.disk_hash {
            return Err(external_change_error(
                &self.path,
                &proposed_bytes,
                state.disk_hash,
                current_hash,
            ));
        }

        if let Some(bytes) = current_bytes.as_deref() {
            create_regular_backup(&self.path, bytes)?;
        }
        let hash_before_replace = read_optional_file(&self.path, "再次核对甘特图数据文件")?
            .as_deref()
            .map(hash_bytes);
        if hash_before_replace != state.disk_hash {
            return Err(external_change_error(
                &self.path,
                &proposed_bytes,
                state.disk_hash,
                hash_before_replace,
            ));
        }
        atomic_write(&self.path, &proposed_bytes)?;
        state.disk_hash = Some(hash_bytes(&proposed_bytes));
        state.document = document;
        Ok(())
    }
}

#[tauri::command]
pub fn list_gantt_tasks(store: State<'_, GanttStore>) -> Vec<GanttTask> {
    store.list()
}

#[tauri::command]
pub async fn upsert_gantt_task(
    app: AppHandle,
    request: UpsertGanttTaskRequest,
) -> AppResult<GanttTask> {
    crate::commands::run_background("保存甘特图任务", move || {
        let task = app.state::<GanttStore>().upsert(request)?;
        let _ = app.emit(
            "gantt_changed",
            json!({ "id": task.id, "kind": "upserted", "task": task }),
        );
        Ok(task)
    })
    .await
}

#[tauri::command]
pub async fn delete_gantt_task(app: AppHandle, task_id: String) -> AppResult<()> {
    crate::commands::run_background("删除甘特图任务", move || {
        app.state::<GanttStore>().delete(&task_id)?;
        let _ = app.emit("gantt_changed", json!({ "id": task_id, "kind": "deleted" }));
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn reorder_gantt_tasks(app: AppHandle, ordered_ids: Vec<String>) -> AppResult<()> {
    crate::commands::run_background("调整甘特图任务顺序", move || {
        app.state::<GanttStore>().reorder(ordered_ids.clone())?;
        let _ = app.emit(
            "gantt_changed",
            json!({ "kind": "reordered", "orderedIds": ordered_ids }),
        );
        Ok(())
    })
    .await
}

fn validate_task_fields(
    name: &str,
    progress: u8,
    start_date: &str,
    start_hour: u8,
    end_date: &str,
    end_hour: u8,
) -> AppResult<(String, String, String)> {
    let name = name.trim();
    let name_length = name.chars().count();
    if !(1..=MAX_NAME_CHARS).contains(&name_length) {
        return Err(AppError::invalid("任务名称长度必须为 1 到 200 个字符"));
    }
    if progress > 100 {
        return Err(AppError::invalid("任务进度必须在 0 到 100 之间"));
    }
    if start_hour > 23 || end_hour > 23 {
        return Err(AppError::invalid("任务小时必须在 0 到 23 之间"));
    }
    let start = parse_date(start_date)?;
    let end = parse_date(end_date)?;
    if start > end || (start == end && start_hour > end_hour) {
        return Err(AppError::invalid("任务开始时间不能晚于结束时间"));
    }
    Ok((
        name.to_string(),
        start.format(DATE_FORMAT).to_string(),
        end.format(DATE_FORMAT).to_string(),
    ))
}

fn parse_date(value: &str) -> AppResult<NaiveDate> {
    let parsed = NaiveDate::parse_from_str(value, DATE_FORMAT)
        .map_err(|_| AppError::invalid("任务日期必须使用 YYYY-MM-DD 格式"))?;
    if parsed.format(DATE_FORMAT).to_string() != value {
        return Err(AppError::invalid("任务日期必须使用 YYYY-MM-DD 格式"));
    }
    Ok(parsed)
}

fn validate_task_id(id: &str) -> AppResult<()> {
    let parsed = Uuid::parse_str(id).map_err(|_| AppError::invalid("甘特图任务 ID 无效"))?;
    if parsed.to_string() != id {
        return Err(AppError::invalid("甘特图任务 ID 格式无效"));
    }
    Ok(())
}

fn normalize_loaded_task(task: &mut GanttTask) -> AppResult<bool> {
    validate_task_id(&task.id)?;
    let (name, start_date, end_date) = validate_task_fields(
        &task.name,
        task.progress,
        &task.start_date,
        task.start_hour,
        &task.end_date,
        task.end_hour,
    )?;
    let changed = task.name != name || task.start_date != start_date || task.end_date != end_date;
    task.name = name;
    task.start_date = start_date;
    task.end_date = end_date;
    Ok(changed)
}

fn decode_document(bytes: &[u8]) -> AppResult<DecodedDocument> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes)?;
    if value.is_array() {
        let values = serde_json::from_value::<Vec<serde_json::Value>>(value)?;
        let (tasks, _) = decode_task_values(values)?;
        return Ok(DecodedDocument {
            document: migrated_document(tasks),
            rewrite_required: true,
        });
    }

    let mut document = serde_json::from_value::<GanttDocument>(value)?;
    if document.schema_version != GANTT_SCHEMA_VERSION {
        return Err(AppError::new(
            "unsupported_schema",
            format!("不支持的甘特图数据版本 {}", document.schema_version),
        ));
    }
    validate_document_id(&document.document_id)?;
    DateTime::parse_from_rfc3339(&document.updated_at)
        .map_err(|_| AppError::invalid("甘特图文档更新时间无效"))?;

    let values = document
        .tasks
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    let (tasks, tasks_changed) = decode_task_values(values)?;
    document.tasks = tasks;

    Ok(DecodedDocument {
        document,
        rewrite_required: tasks_changed,
    })
}

fn decode_task_values(values: Vec<serde_json::Value>) -> AppResult<(Vec<GanttTask>, bool)> {
    let mut tasks = Vec::with_capacity(values.len());
    let mut seen_ids = HashSet::with_capacity(values.len());
    let mut rewrite_required = false;

    for value in values {
        let migrated_hours = value.get("startHour").is_none() || value.get("endHour").is_none();
        let mut task = serde_json::from_value::<GanttTask>(value)?;
        let changed = normalize_loaded_task(&mut task)?;
        if !seen_ids.insert(task.id.clone()) {
            return Err(AppError::invalid("甘特图数据包含重复的任务 ID"));
        }
        rewrite_required |= migrated_hours || changed;
        tasks.push(task);
    }

    Ok((tasks, rewrite_required))
}

fn validate_document_id(id: &str) -> AppResult<()> {
    let parsed = Uuid::parse_str(id).map_err(|_| AppError::invalid("甘特图文档 ID 无效"))?;
    if parsed.to_string() != id {
        return Err(AppError::invalid("甘特图文档 ID 格式无效"));
    }
    Ok(())
}

fn empty_document() -> GanttDocument {
    GanttDocument {
        schema_version: GANTT_SCHEMA_VERSION,
        document_id: Uuid::new_v4().to_string(),
        revision: 0,
        updated_at: now(),
        tasks: Vec::new(),
    }
}

fn migrated_document(tasks: Vec<GanttTask>) -> GanttDocument {
    GanttDocument {
        schema_version: GANTT_SCHEMA_VERSION,
        document_id: Uuid::new_v4().to_string(),
        revision: 1,
        updated_at: now(),
        tasks,
    }
}

fn advance_document(document: &mut GanttDocument, updated_at: String) -> AppResult<()> {
    document.revision = document
        .revision
        .checked_add(1)
        .ok_or_else(|| AppError::new("revision_overflow", "甘特图数据版本已达到上限"))?;
    document.updated_at = updated_at;
    Ok(())
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn load_document(path: &Path) -> AppResult<(GanttDocument, Option<[u8; 32]>, bool)> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some((document, backup_bytes)) = newest_valid_backup(path)? {
                atomic_write(path, &backup_bytes)?;
                return Ok((document, Some(hash_bytes(&backup_bytes)), false));
            }
            return Ok((empty_document(), None, false));
        }
        Err(error) => return Err(AppError::io("读取甘特图数据", error)),
    };

    match decode_document(&bytes) {
        Ok(decoded) => {
            if !decoded.rewrite_required {
                return Ok((decoded.document, Some(hash_bytes(&bytes)), false));
            }
            preserve_migration_backup(path, &bytes)?;
            let normalized = serde_json::to_vec_pretty(&decoded.document)?;
            atomic_write(path, &normalized)?;
            return Ok((decoded.document, Some(hash_bytes(&normalized)), false));
        }
        Err(error) if error.code == "unsupported_schema" => {
            return Ok((empty_document(), Some(hash_bytes(&bytes)), true));
        }
        Err(_) => {}
    }

    preserve_corrupt_bytes(path, &bytes)?;
    if let Some((document, backup_bytes)) = newest_valid_backup(path)? {
        atomic_write(path, &backup_bytes)?;
        return Ok((document, Some(hash_bytes(&backup_bytes)), false));
    }

    Ok((empty_document(), Some(hash_bytes(&bytes)), true))
}

fn migrate_legacy_file(path: &Path, legacy_path: Option<&Path>) -> AppResult<()> {
    if path.exists() {
        return Ok(());
    }
    let Some(legacy_path) = legacy_path.filter(|legacy| legacy.is_file()) else {
        return Ok(());
    };
    let bytes = fs::read(legacy_path).map_err(|error| AppError::io("读取旧甘特图数据", error))?;
    let decoded = match decode_document(&bytes) {
        Ok(decoded) => decoded,
        Err(_) => {
            preserve_corrupt_bytes(path, &bytes)?;
            return Ok(());
        }
    };

    preserve_migration_backup(path, &bytes)?;
    let migrated = serde_json::to_vec_pretty(&decoded.document)?;
    atomic_write(path, &migrated)?;
    let _ = fs::remove_file(legacy_path);
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hash_hex(hash: [u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn timestamped_name(kind: &str) -> String {
    format!(
        "gantt.{kind}-{}-{}.json",
        Utc::now().format("%Y%m%dT%H%M%S%3fZ"),
        Uuid::new_v4()
    )
}

fn preserve_migration_backup(path: &Path, bytes: &[u8]) -> AppResult<PathBuf> {
    let directory = data_subdirectory(path, &["backups", "migrations"])?;
    let backup = directory.join(timestamped_name("migration"));
    atomic_write(&backup, bytes)?;
    Ok(backup)
}

fn preserve_corrupt_bytes(path: &Path, bytes: &[u8]) -> AppResult<PathBuf> {
    let directory = data_subdirectory(path, &["corrupt"])?;
    let fingerprint = hash_hex(hash_bytes(bytes));
    let backup = directory.join(format!("gantt.corrupt-{}.json", &fingerprint[..16]));
    if !backup.exists() {
        atomic_write(&backup, bytes)?;
    }
    Ok(backup)
}

fn preserve_conflict_document(path: &Path, bytes: &[u8]) -> AppResult<PathBuf> {
    let directory = data_subdirectory(path, &["conflicts"])?;
    let conflict = directory.join(timestamped_name("conflict"));
    atomic_write(&conflict, bytes)?;
    Ok(conflict)
}

fn pending_snapshot_details(path: &Path, bytes: &[u8]) -> (serde_json::Value, bool) {
    match preserve_conflict_document(path, bytes) {
        Ok(conflict_path) => (json!({ "conflictPath": conflict_path }), true),
        Err(error) => (json!({ "conflictSaveError": error.message }), false),
    }
}

fn external_change_error(
    path: &Path,
    proposed_bytes: &[u8],
    expected_hash: Option<[u8; 32]>,
    actual_hash: Option<[u8; 32]>,
) -> AppError {
    let (mut details, conflict_saved) = pending_snapshot_details(path, proposed_bytes);
    if let Some(object) = details.as_object_mut() {
        object.insert(
            "expectedHash".to_string(),
            json!(expected_hash.map(hash_hex)),
        );
        object.insert("actualHash".to_string(), json!(actual_hash.map(hash_hex)));
    }
    let message = if conflict_saved {
        "甘特图数据已被外部程序修改，未覆盖磁盘文件；本次修改已另存到冲突目录"
    } else {
        "甘特图数据已被外部程序修改，未覆盖磁盘文件；本次修改也未能保存到冲突目录"
    };
    AppError::new("gantt_conflict", message).with_details(details)
}

fn read_optional_file(path: &Path, action: &str) -> AppResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::io(action, error)),
    }
}

fn create_regular_backup(path: &Path, bytes: &[u8]) -> AppResult<()> {
    decode_document(bytes).map_err(|_| {
        AppError::new(
            "gantt_corrupt",
            "甘特图主文件未通过校验，已拒绝创建备份和覆盖",
        )
    })?;
    let directory = data_subdirectory(path, &["backups"])?;
    let backup = directory.join(timestamped_name("backup"));
    atomic_write(&backup, bytes)?;
    // Retention maintenance is not part of the authoritative task write. The
    // newly created backup is already valid, so a stale file that cannot be
    // pruned must not make the following task save fail.
    let _ = prune_regular_backups(&directory);
    Ok(())
}

fn newest_valid_backup(path: &Path) -> AppResult<Option<(GanttDocument, Vec<u8>)>> {
    let directory = match path.parent() {
        Some(parent) => parent.join("backups"),
        None => return Err(AppError::invalid("甘特图数据文件没有父目录")),
    };
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("读取甘特图备份目录", error)),
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| is_regular_backup(candidate))
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.file_name().cmp(&left.file_name()));

    for candidate in paths {
        let Ok(bytes) = fs::read(&candidate) else {
            continue;
        };
        let Ok(decoded) = decode_document(&bytes) else {
            continue;
        };
        let normalized = if decoded.rewrite_required {
            serde_json::to_vec_pretty(&decoded.document)?
        } else {
            bytes
        };
        return Ok(Some((decoded.document, normalized)));
    }
    Ok(None)
}

fn prune_regular_backups(directory: &Path) -> AppResult<()> {
    let mut paths = fs::read_dir(directory)
        .map_err(|error| AppError::io("读取甘特图备份目录", error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| is_regular_backup(candidate))
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    for stale in paths.into_iter().skip(MAX_BACKUPS) {
        fs::remove_file(stale).map_err(|error| AppError::io("清理旧甘特图备份", error))?;
    }
    Ok(())
}

fn is_regular_backup(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("gantt.backup-") && name.ends_with(".json"))
}

fn data_subdirectory(path: &Path, segments: &[&str]) -> AppResult<PathBuf> {
    let mut directory = path
        .parent()
        .ok_or_else(|| AppError::invalid("甘特图数据文件没有父目录"))?
        .to_path_buf();
    for segment in segments {
        directory.push(segment);
    }
    fs::create_dir_all(&directory)
        .map_err(|error| AppError::io("创建甘特图辅助数据目录", error))?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_uses_workspace_tools_directory() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::load(root.path()).unwrap();
        let expected = root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("gantt")
            .join("gantt.json");

        assert_eq!(store.path, expected);
        store
            .upsert_at(
                request("目录测试", 0, "2026-07-01", "2026-07-01"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        assert!(expected.is_file());
        assert!(!root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("gantt.json")
            .exists());
    }

    fn request(
        name: &str,
        progress: u8,
        start_date: &str,
        end_date: &str,
    ) -> UpsertGanttTaskRequest {
        UpsertGanttTaskRequest {
            id: None,
            name: name.to_string(),
            progress,
            start_date: start_date.to_string(),
            start_hour: 0,
            end_date: end_date.to_string(),
            end_hour: 23,
        }
    }

    fn request_with_hours(
        name: &str,
        progress: u8,
        start_date: &str,
        start_hour: u8,
        end_date: &str,
        end_hour: u8,
    ) -> UpsertGanttTaskRequest {
        UpsertGanttTaskRequest {
            id: None,
            name: name.to_string(),
            progress,
            start_date: start_date.to_string(),
            start_hour,
            end_date: end_date.to_string(),
            end_hour,
        }
    }

    #[test]
    fn persists_crud_and_reloads_tasks() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let created = store
            .upsert_at(
                request_with_hours("设计首页", 25, "2026-07-01", 9, "2026-07-05", 18),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        assert_eq!(store.list(), vec![created.clone()]);
        assert_eq!((created.start_hour, created.end_hour), (9, 18));

        let mut update = request_with_hours("完成首页", 100, "2026-07-02", 10, "2026-07-06", 16);
        update.id = Some(created.id.clone());
        let updated = store
            .upsert_at(update, "2026-07-02T08:00:00.000Z".to_string())
            .unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.created_at, created.created_at);
        assert_eq!(updated.name, "完成首页");
        assert_eq!((updated.start_hour, updated.end_hour), (10, 16));

        let reloaded = GanttStore::for_test(root.path()).unwrap();
        assert_eq!(reloaded.list(), vec![updated.clone()]);
        reloaded.delete(&updated.id).unwrap();
        assert!(GanttStore::for_test(root.path()).unwrap().list().is_empty());
    }

    #[test]
    fn one_change_advances_revision_once_and_an_identical_update_is_a_noop() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let created = store
            .upsert_at(
                request("一次改动", 10, "2026-07-01", "2026-07-02"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        let path = root.path().join("gantt").join("gantt.json");
        let first = decode_document(&fs::read(&path).unwrap()).unwrap().document;
        assert_eq!(first.revision, 1);

        let mut identical = request("一次改动", 10, "2026-07-01", "2026-07-02");
        identical.id = Some(created.id);
        store
            .upsert_at(identical, "2026-07-01T09:00:00.000Z".to_string())
            .unwrap();
        let unchanged = decode_document(&fs::read(&path).unwrap()).unwrap().document;
        assert_eq!(unchanged.revision, 1);
        assert_eq!(unchanged.updated_at, first.updated_at);
    }

    #[test]
    fn refuses_to_overwrite_an_external_replacement_and_saves_pending_snapshot() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let created = store
            .upsert_at(
                request("内存版本", 10, "2026-07-01", "2026-07-02"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        let path = root.path().join("gantt").join("gantt.json");
        let mut external = decode_document(&fs::read(&path).unwrap()).unwrap().document;
        external.revision += 10;
        external.updated_at = "2026-07-01T08:30:00.000Z".to_string();
        external.tasks[0].name = "外部版本".to_string();
        let external_bytes = serde_json::to_vec_pretty(&external).unwrap();
        atomic_write(&path, &external_bytes).unwrap();

        let mut update = request("本地待保存版本", 50, "2026-07-01", "2026-07-02");
        update.id = Some(created.id);
        let error = store
            .upsert_at(update, "2026-07-01T09:00:00.000Z".to_string())
            .unwrap_err();
        assert_eq!(error.code, "gantt_conflict");
        assert_eq!(fs::read(&path).unwrap(), external_bytes);
        assert_eq!(store.list()[0].name, "内存版本");

        let conflicts = fs::read_dir(root.path().join("gantt").join("conflicts"))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(conflicts.len(), 1);
        let pending = decode_document(&fs::read(conflicts[0].path()).unwrap())
            .unwrap()
            .document;
        assert_eq!(pending.tasks[0].name, "本地待保存版本");
    }

    #[test]
    fn conflict_error_does_not_claim_a_snapshot_was_saved_when_the_directory_is_blocked() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let created = store
            .upsert_at(
                request("内存版本", 10, "2026-07-01", "2026-07-02"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        let path = root.path().join("gantt").join("gantt.json");
        let mut external = decode_document(&fs::read(&path).unwrap()).unwrap().document;
        external.revision += 1;
        external.updated_at = "2026-07-01T08:30:00.000Z".to_string();
        atomic_write(&path, &serde_json::to_vec_pretty(&external).unwrap()).unwrap();
        fs::write(root.path().join("gantt").join("conflicts"), b"blocked").unwrap();

        let mut update = request("待保存版本", 50, "2026-07-01", "2026-07-02");
        update.id = Some(created.id);
        let error = store
            .upsert_at(update, "2026-07-01T09:00:00.000Z".to_string())
            .unwrap_err();

        assert_eq!(error.code, "gantt_conflict");
        assert!(error.message.contains("未能保存到冲突目录"));
        assert!(error.details.unwrap().get("conflictSaveError").is_some());
    }

    #[test]
    fn restores_the_newest_valid_backup_when_the_primary_is_corrupt() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let first = store
            .upsert_at(
                request("可恢复版本", 10, "2026-07-01", "2026-07-02"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        let second = store
            .upsert_at(
                request("较新版本", 20, "2026-07-02", "2026-07-03"),
                "2026-07-01T09:00:00.000Z".to_string(),
            )
            .unwrap();
        let path = root.path().join("gantt").join("gantt.json");
        fs::write(&path, b"{broken primary").unwrap();

        let recovered = GanttStore::for_test(root.path()).unwrap();
        assert_eq!(recovered.list(), vec![first]);
        assert!(!recovered.list().contains(&second));
        let restored = decode_document(&fs::read(&path).unwrap()).unwrap().document;
        assert_eq!(restored.tasks, recovered.list());
        assert_eq!(
            fs::read_dir(root.path().join("gantt").join("corrupt"))
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn restores_the_newest_valid_backup_when_the_primary_is_missing() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let first = store
            .upsert_at(
                request("缺失恢复版本", 10, "2026-07-01", "2026-07-02"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        store
            .upsert_at(
                request("用于产生备份", 20, "2026-07-02", "2026-07-03"),
                "2026-07-01T09:00:00.000Z".to_string(),
            )
            .unwrap();
        let path = root.path().join("gantt").join("gantt.json");
        fs::remove_file(&path).unwrap();

        let recovered = GanttStore::for_test(root.path()).unwrap();
        assert_eq!(recovered.list(), vec![first]);
        assert!(path.exists());
    }

    #[test]
    fn keeps_only_the_five_newest_regular_backups() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        for index in 0..7 {
            store
                .upsert_at(
                    request(&format!("任务 {index}"), index, "2026-07-01", "2026-07-02"),
                    format!("2026-07-01T08:00:{index:02}.000Z"),
                )
                .unwrap();
        }
        assert_eq!(
            fs::read_dir(root.path().join("gantt").join("backups"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| is_regular_backup(path))
                .count(),
            MAX_BACKUPS
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_unprunable_old_backup_does_not_block_the_task_save() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        for index in 0..=MAX_BACKUPS {
            store
                .upsert_at(
                    request(
                        &format!("任务 {index}"),
                        index as u8,
                        "2026-07-01",
                        "2026-07-02",
                    ),
                    format!("2026-07-01T08:00:{index:02}.000Z"),
                )
                .unwrap();
        }
        let backup_dir = root.path().join("gantt").join("backups");
        let oldest = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_regular_backup(path))
            .min_by(|left, right| left.file_name().cmp(&right.file_name()))
            .unwrap();
        let held_backup = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&oldest)
            .unwrap();

        let saved = store
            .upsert_at(
                request("清理失败也要保存", 80, "2026-07-03", "2026-07-04"),
                "2026-07-01T09:00:00.000Z".to_string(),
            )
            .unwrap();

        assert_eq!(store.list().last().unwrap(), &saved);
        assert!(oldest.exists());
        drop(held_backup);
    }

    #[test]
    fn reorders_tasks_and_persists_the_new_order() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let first = store
            .upsert_at(
                request("任务一", 0, "2026-07-01", "2026-07-02"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        let second = store
            .upsert_at(
                request("任务二", 50, "2026-07-02", "2026-07-03"),
                "2026-07-01T09:00:00.000Z".to_string(),
            )
            .unwrap();
        let third = store
            .upsert_at(
                request("任务三", 100, "2026-07-03", "2026-07-04"),
                "2026-07-01T10:00:00.000Z".to_string(),
            )
            .unwrap();

        let ordered_ids = vec![third.id.clone(), first.id.clone(), second.id.clone()];
        store.reorder(ordered_ids.clone()).unwrap();

        assert_eq!(
            store
                .list()
                .into_iter()
                .map(|task| task.id)
                .collect::<Vec<_>>(),
            ordered_ids
        );
        assert_eq!(
            GanttStore::for_test(root.path()).unwrap().list(),
            vec![third, first, second]
        );
    }

    #[test]
    fn rejects_invalid_reorders_without_changing_the_existing_order() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let tasks = ["任务一", "任务二", "任务三"]
            .into_iter()
            .map(|name| {
                store
                    .upsert_at(
                        request(name, 0, "2026-07-01", "2026-07-02"),
                        "2026-07-01T08:00:00.000Z".to_string(),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let original_ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();

        let invalid_orders = [
            vec![original_ids[0].clone(), original_ids[1].clone()],
            vec![
                original_ids[0].clone(),
                original_ids[0].clone(),
                original_ids[2].clone(),
            ],
            vec![
                original_ids[0].clone(),
                original_ids[1].clone(),
                Uuid::new_v4().to_string(),
            ],
            vec![
                "550E8400-E29B-41D4-A716-446655440000".to_string(),
                original_ids[1].clone(),
                original_ids[2].clone(),
            ],
        ];

        for ordered_ids in invalid_orders {
            assert_eq!(
                store.reorder(ordered_ids).unwrap_err().code,
                "invalid_input"
            );
            assert_eq!(
                store
                    .list()
                    .into_iter()
                    .map(|task| task.id)
                    .collect::<Vec<_>>(),
                original_ids
            );
        }
        assert_eq!(GanttStore::for_test(root.path()).unwrap().list(), tasks);
    }

    #[test]
    fn validates_name_progress_dates_and_ids() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();

        assert_eq!(
            store
                .upsert_at(request("   ", 0, "2026-07-01", "2026-07-01"), now())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(
                    request(&"任".repeat(201), 0, "2026-07-01", "2026-07-01"),
                    now()
                )
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(request("任务", 101, "2026-07-01", "2026-07-01"), now())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(request("任务", 0, "2026-07-02", "2026-07-01"), now())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(request("任务", 0, "2026-7-01", "2026-07-01"), now())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(
                    request_with_hours("任务", 0, "2026-07-01", 24, "2026-07-01", 23),
                    now()
                )
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(
                    request_with_hours("任务", 0, "2026-07-01", 0, "2026-07-01", 24),
                    now()
                )
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            store
                .upsert_at(
                    request_with_hours("任务", 0, "2026-07-01", 18, "2026-07-01", 9),
                    now()
                )
                .unwrap_err()
                .code,
            "invalid_input"
        );

        let mut invalid_id = request("任务", 0, "2026-07-01", "2026-07-01");
        invalid_id.id = Some("../task".to_string());
        assert_eq!(
            store.upsert_at(invalid_id, now()).unwrap_err().code,
            "invalid_input"
        );

        let mut noncanonical_id = request("任务", 0, "2026-07-01", "2026-07-01");
        noncanonical_id.id = Some("550E8400-E29B-41D4-A716-446655440000".to_string());
        assert_eq!(
            store.upsert_at(noncanonical_id, now()).unwrap_err().code,
            "invalid_input"
        );
    }

    #[test]
    fn rejects_missing_task_on_update_and_delete() {
        let root = TempDir::new().unwrap();
        let store = GanttStore::for_test(root.path()).unwrap();
        let id = Uuid::new_v4().to_string();
        let mut update = request("任务", 0, "2026-07-01", "2026-07-01");
        update.id = Some(id.clone());
        assert_eq!(store.upsert(update).unwrap_err().code, "not_found");
        assert_eq!(store.delete(&id).unwrap_err().code, "not_found");
    }

    #[test]
    fn preserves_corrupt_data_and_refuses_to_overwrite_without_a_backup() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("gantt");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("gantt.json");
        fs::write(&path, b"{not valid json").unwrap();

        let store = GanttStore::for_test(root.path()).unwrap();
        assert!(store.list().is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"{not valid json");
        assert_eq!(
            store
                .upsert_at(
                    request("不得覆盖", 0, "2026-07-01", "2026-07-01"),
                    "2026-07-01T08:00:00.000Z".to_string(),
                )
                .unwrap_err()
                .code,
            "gantt_corrupt"
        );
        assert_eq!(fs::read(&path).unwrap(), b"{not valid json");
        let corrupt = fs::read_dir(directory.join("corrupt"))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(corrupt.len(), 1);
        assert_eq!(fs::read(corrupt[0].path()).unwrap(), b"{not valid json");
        assert_eq!(
            fs::read_dir(directory.join("conflicts"))
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn migrates_legacy_tasks_with_default_hours_and_rewrites_the_file() {
        let root = TempDir::new().unwrap();
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        let legacy = json!({
            "id": id,
            "name": "旧任务",
            "progress": 10,
            "startDate": "2026-07-01",
            "endDate": "2026-07-02",
            "createdAt": timestamp,
            "updatedAt": timestamp,
        });
        let path = root.path().join("gantt.json");
        let legacy_bytes = serde_json::to_vec_pretty(&json!([legacy])).unwrap();
        fs::write(&path, &legacy_bytes).unwrap();

        let store = GanttStore::for_test(root.path()).unwrap();
        let tasks = store.list();
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].start_hour, tasks[0].end_hour), (0, 23));

        let migrated_path = root.path().join("gantt").join("gantt.json");
        let persisted = decode_document(&fs::read(&migrated_path).unwrap())
            .unwrap()
            .document;
        assert_eq!(persisted.schema_version, GANTT_SCHEMA_VERSION);
        assert_eq!(persisted.revision, 1);
        assert_eq!(persisted.tasks, tasks);
        assert!(!path.exists());
        let migration_backups =
            fs::read_dir(root.path().join("gantt").join("backups").join("migrations"))
                .unwrap()
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
        assert_eq!(migration_backups.len(), 1);
        assert_eq!(fs::read(migration_backups[0].path()).unwrap(), legacy_bytes);

        let reloaded = GanttStore::for_test(root.path()).unwrap();
        assert_eq!(reloaded.list(), tasks);
        assert_eq!(
            fs::read_dir(root.path().join("gantt").join("backups").join("migrations"),)
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn refuses_partial_legacy_corruption_instead_of_silently_dropping_entries() {
        let root = TempDir::new().unwrap();
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        let valid = json!({
            "id": id,
            "name": "有效任务",
            "progress": 10,
            "startDate": "2026-07-01",
            "endDate": "2026-07-02",
            "createdAt": timestamp,
            "updatedAt": timestamp,
        });
        let legacy_bytes = serde_json::to_vec_pretty(&json!([valid, {"id": "bad"}])).unwrap();
        fs::write(root.path().join("gantt.json"), &legacy_bytes).unwrap();

        let store = GanttStore::for_test(root.path()).unwrap();
        assert!(store.list().is_empty());
        assert!(root.path().join("gantt.json").exists());
        assert!(!root.path().join("gantt").join("gantt.json").exists());
        store
            .upsert_at(
                request("新格式任务", 0, "2026-07-01", "2026-07-01"),
                "2026-07-01T08:00:00.000Z".to_string(),
            )
            .unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(
            fs::read_dir(root.path().join("gantt").join("corrupt"))
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );
    }
}
