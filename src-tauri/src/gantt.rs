use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write_json, INTERNAL_DATA_DIR};
use chrono::{NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

const DATE_FORMAT: &str = "%Y-%m-%d";
const MAX_NAME_CHARS: usize = 200;

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

#[derive(Debug)]
struct DecodedTasks {
    tasks: Vec<GanttTask>,
    rewrite_required: bool,
    recovered_corruption: bool,
}

pub struct GanttStore {
    path: PathBuf,
    tasks: Mutex<Vec<GanttTask>>,
}

impl GanttStore {
    pub fn load(root: &Path) -> AppResult<Self> {
        Self::load_from(
            &root
                .join(INTERNAL_DATA_DIR)
                .join("tools")
                .join("gantt.json"),
        )
    }

    fn load_from(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| AppError::io("创建甘特图数据目录", error))?;
        }

        let tasks = match fs::read(path) {
            Ok(bytes) => match decode_tasks(&bytes) {
                Ok(decoded) => {
                    if decoded.recovered_corruption {
                        preserve_and_replace_corrupt_file(path, &decoded.tasks);
                    } else if decoded.rewrite_required {
                        atomic_write_json(path, &decoded.tasks)?;
                    }
                    decoded.tasks
                }
                Err(_) => {
                    let tasks = Vec::new();
                    preserve_and_replace_corrupt_file(path, &tasks);
                    tasks
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(AppError::io("读取甘特图数据", error)),
        };

        Ok(Self {
            path: path.to_path_buf(),
            tasks: Mutex::new(tasks),
        })
    }

    #[cfg(test)]
    fn for_test(app_data: &Path) -> AppResult<Self> {
        Self::load_from(&app_data.join("gantt.json"))
    }

    pub fn list(&self) -> Vec<GanttTask> {
        self.tasks
            .lock()
            .expect("gantt store lock poisoned")
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

        let mut current = self.tasks.lock().expect("gantt store lock poisoned");
        let mut tasks = current.clone();
        let task = if let Some(id) = request.id {
            validate_task_id(&id)?;
            let task = tasks
                .iter_mut()
                .find(|task| task.id == id)
                .ok_or_else(|| AppError::not_found("没有找到这个甘特图任务"))?;
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
            tasks.push(task.clone());
            task
        };

        self.persist(&tasks)?;
        *current = tasks;
        Ok(task)
    }

    pub fn delete(&self, task_id: &str) -> AppResult<()> {
        validate_task_id(task_id)?;
        let mut current = self.tasks.lock().expect("gantt store lock poisoned");
        let mut tasks = current.clone();
        let original_len = tasks.len();
        tasks.retain(|task| task.id != task_id);
        if tasks.len() == original_len {
            return Err(AppError::not_found("没有找到这个甘特图任务"));
        }
        self.persist(&tasks)?;
        *current = tasks;
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

        let mut current = self.tasks.lock().expect("gantt store lock poisoned");
        if ordered_ids.len() != current.len() {
            return Err(AppError::invalid("甘特图任务排序必须包含全部任务 ID"));
        }

        let existing_ids = current
            .iter()
            .map(|task| task.id.as_str())
            .collect::<HashSet<_>>();
        if requested_ids != existing_ids {
            return Err(AppError::invalid("甘特图任务排序包含未知或缺失的 ID"));
        }

        let mut tasks_by_id = current
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

        self.persist(&tasks)?;
        *current = tasks;
        Ok(())
    }

    fn persist(&self, tasks: &[GanttTask]) -> AppResult<()> {
        atomic_write_json(&self.path, tasks)
    }
}

#[tauri::command]
pub fn list_gantt_tasks(store: State<'_, GanttStore>) -> Vec<GanttTask> {
    store.list()
}

#[tauri::command]
pub fn upsert_gantt_task(
    app: AppHandle,
    store: State<'_, GanttStore>,
    request: UpsertGanttTaskRequest,
) -> AppResult<GanttTask> {
    let task = store.upsert(request)?;
    let _ = app.emit(
        "gantt_changed",
        json!({ "id": task.id, "kind": "upserted", "task": task }),
    );
    Ok(task)
}

#[tauri::command]
pub fn delete_gantt_task(
    app: AppHandle,
    store: State<'_, GanttStore>,
    task_id: String,
) -> AppResult<()> {
    store.delete(&task_id)?;
    let _ = app.emit("gantt_changed", json!({ "id": task_id, "kind": "deleted" }));
    Ok(())
}

#[tauri::command]
pub fn reorder_gantt_tasks(
    app: AppHandle,
    store: State<'_, GanttStore>,
    ordered_ids: Vec<String>,
) -> AppResult<()> {
    store.reorder(ordered_ids.clone())?;
    let _ = app.emit(
        "gantt_changed",
        json!({ "kind": "reordered", "orderedIds": ordered_ids }),
    );
    Ok(())
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

fn decode_tasks(bytes: &[u8]) -> Result<DecodedTasks, serde_json::Error> {
    let values = serde_json::from_slice::<Vec<serde_json::Value>>(bytes)?;
    let mut tasks = Vec::with_capacity(values.len());
    let mut seen_ids = HashSet::with_capacity(values.len());
    let mut rewrite_required = false;
    let mut recovered_corruption = false;

    for value in values {
        let migrated_hours = value.get("startHour").is_none() || value.get("endHour").is_none();
        let Ok(mut task) = serde_json::from_value::<GanttTask>(value) else {
            recovered_corruption = true;
            continue;
        };
        match normalize_loaded_task(&mut task) {
            Ok(changed) => {
                if seen_ids.insert(task.id.clone()) {
                    rewrite_required |= migrated_hours || changed;
                    tasks.push(task);
                } else {
                    recovered_corruption = true;
                }
            }
            Err(_) => recovered_corruption = true,
        }
    }

    Ok(DecodedTasks {
        tasks,
        rewrite_required,
        recovered_corruption,
    })
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn preserve_and_replace_corrupt_file(path: &Path, tasks: &[GanttTask]) {
    if preserve_corrupt_file(path).is_ok() {
        let _ = atomic_write_json(path, tasks);
    }
}

fn preserve_corrupt_file(path: &Path) -> AppResult<()> {
    let Some(parent) = path.parent() else {
        return Err(AppError::invalid("甘特图数据文件没有父目录"));
    };
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let backup = parent.join(format!("gantt.corrupt-{timestamp}-{}.json", Uuid::new_v4()));
    if fs::rename(path, &backup).is_ok() {
        return Ok(());
    }
    fs::copy(path, &backup)
        .map(|_| ())
        .map_err(|error| AppError::io("备份损坏的甘特图数据", error))
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
            .join("gantt.json");

        assert_eq!(store.path, expected);
        store.persist(&[]).unwrap();
        assert!(expected.is_file());
        assert!(!root.path().join("gantt.json").exists());
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
    fn backs_up_corrupt_data_and_starts_empty() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("gantt.json");
        fs::write(&path, b"{not valid json").unwrap();

        let store = GanttStore::for_test(root.path()).unwrap();
        assert!(store.list().is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"[]");
        let backups = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("gantt.corrupt-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), b"{not valid json");
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
        fs::write(&path, serde_json::to_vec_pretty(&json!([legacy])).unwrap()).unwrap();

        let store = GanttStore::for_test(root.path()).unwrap();
        let tasks = store.list();
        assert_eq!(tasks.len(), 1);
        assert_eq!((tasks[0].start_hour, tasks[0].end_hour), (0, 23));

        let persisted =
            serde_json::from_slice::<Vec<serde_json::Value>>(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted[0]["startHour"], 0);
        assert_eq!(persisted[0]["endHour"], 23);
        assert!(!fs::read_dir(root.path()).unwrap().any(|entry| {
            entry.is_ok_and(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("gantt.corrupt-")
            })
        }));
    }

    #[test]
    fn recovers_legacy_valid_entries_when_some_entries_are_invalid() {
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
        fs::write(
            root.path().join("gantt.json"),
            serde_json::to_vec_pretty(&json!([valid, {"id": "bad"}])).unwrap(),
        )
        .unwrap();

        let store = GanttStore::for_test(root.path()).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(
            (store.list()[0].start_hour, store.list()[0].end_hour),
            (0, 23)
        );
        let persisted = serde_json::from_slice::<Vec<GanttTask>>(
            &fs::read(root.path().join("gantt.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted, store.list());
    }
}
