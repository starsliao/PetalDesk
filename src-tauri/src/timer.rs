use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write_json, INTERNAL_DATA_DIR};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};

pub const TIMER_DATA_VERSION: u32 = 1;
pub const TIMER_LOG_LIMIT: usize = 500;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_LOG_ID_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TimerAction {
    Reset,
    Pause,
    Resume,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimerLogEntry {
    pub id: String,
    pub timestamp: u64,
    pub action: TimerAction,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimerData {
    pub version: u32,
    pub accumulated_ms: u64,
    pub running_since: Option<u64>,
    #[serde(default)]
    pub logs: Vec<TimerLogEntry>,
    #[serde(default = "default_digit_opacity")]
    pub digit_opacity: f64,
}

impl Default for TimerData {
    fn default() -> Self {
        Self {
            version: TIMER_DATA_VERSION,
            accumulated_ms: 0,
            running_since: None,
            logs: Vec::new(),
            digit_opacity: default_digit_opacity(),
        }
    }
}

pub struct TimerStore {
    path: PathBuf,
    data: Mutex<TimerData>,
}

impl TimerStore {
    pub fn load(root: &Path) -> AppResult<Self> {
        let path = root
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("timer.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| AppError::io("创建计时器数据目录", error))?;
        }

        let data = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<TimerData>(&bytes) {
                Ok(data) if validate_timer_data(&data).is_ok() => data,
                Ok(_) | Err(_) => recover_corrupt_data(&path)?,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => TimerData::default(),
            Err(error) => return Err(AppError::io("读取计时器数据", error)),
        };

        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    pub fn get(&self) -> TimerData {
        self.data.lock().expect("timer store lock poisoned").clone()
    }

    pub fn save(&self, data: TimerData) -> AppResult<TimerData> {
        validate_timer_data(&data)?;
        let mut current = self.data.lock().expect("timer store lock poisoned");
        atomic_write_json(&self.path, &data)?;
        *current = data.clone();
        Ok(data)
    }
}

#[tauri::command]
pub fn get_timer_data(store: State<'_, TimerStore>) -> TimerData {
    store.get()
}

#[tauri::command]
pub async fn save_timer_data(app: AppHandle, data: TimerData) -> AppResult<TimerData> {
    crate::commands::run_background("保存计时器数据", move || {
        app.state::<TimerStore>().save(data)
    })
    .await
}

const fn default_digit_opacity() -> f64 {
    1.0
}

fn validate_timer_data(data: &TimerData) -> AppResult<()> {
    if data.version != TIMER_DATA_VERSION {
        return Err(AppError::invalid("不支持的计时器数据版本"));
    }
    validate_milliseconds(data.accumulated_ms, "累计计时时间")?;
    if let Some(running_since) = data.running_since {
        validate_milliseconds(running_since, "计时器开始时间")?;
    }
    if !data.digit_opacity.is_finite() || !(0.0..=1.0).contains(&data.digit_opacity) {
        return Err(AppError::invalid("计时器数字透明度必须在 0 到 1 之间"));
    }
    if data.logs.len() > TIMER_LOG_LIMIT {
        return Err(AppError::invalid(format!(
            "计时记录不能超过 {TIMER_LOG_LIMIT} 条"
        )));
    }

    let mut ids = HashSet::with_capacity(data.logs.len());
    for entry in &data.logs {
        let id_length = entry.id.chars().count();
        if entry.id.trim().is_empty()
            || entry.id.trim() != entry.id
            || id_length > MAX_LOG_ID_CHARS
            || entry.id.chars().any(char::is_control)
        {
            return Err(AppError::invalid("计时记录 ID 无效"));
        }
        if !ids.insert(entry.id.as_str()) {
            return Err(AppError::invalid("计时记录 ID 不能重复"));
        }
        validate_milliseconds(entry.timestamp, "计时记录时间")?;
        validate_milliseconds(entry.elapsed_ms, "计时记录时长")?;
    }

    Ok(())
}

fn validate_milliseconds(value: u64, field: &str) -> AppResult<()> {
    if value > MAX_SAFE_INTEGER {
        return Err(AppError::invalid(format!(
            "{field}超过 JavaScript 可安全表示的范围"
        )));
    }
    Ok(())
}

fn recover_corrupt_data(path: &Path) -> AppResult<TimerData> {
    preserve_corrupt_file(path)?;
    let data = TimerData::default();
    atomic_write_json(path, &data)?;
    Ok(data)
}

fn preserve_corrupt_file(path: &Path) -> AppResult<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::invalid("计时器数据文件没有父目录"))?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S-%f");
    let mut backup = parent.join(format!("timer.corrupt-{timestamp}.json"));
    let mut suffix = 1_u32;
    while backup.exists() {
        backup = parent.join(format!("timer.corrupt-{timestamp}-{suffix}.json"));
        suffix = suffix.saturating_add(1);
    }

    if fs::rename(path, &backup).is_ok() {
        return Ok(backup);
    }
    fs::copy(path, &backup)
        .map(|_| backup)
        .map_err(|error| AppError::io("备份损坏的计时器数据", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn timer_path(root: &Path) -> PathBuf {
        root.join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("timer.json")
    }

    fn valid_data() -> TimerData {
        TimerData {
            version: TIMER_DATA_VERSION,
            accumulated_ms: 65_432,
            running_since: Some(1_722_222_222_333),
            logs: vec![
                TimerLogEntry {
                    id: "1722222222333-1".to_string(),
                    timestamp: 1_722_222_222_333,
                    action: TimerAction::Reset,
                    elapsed_ms: 0,
                },
                TimerLogEntry {
                    id: "1722222287765-2".to_string(),
                    timestamp: 1_722_222_287_765,
                    action: TimerAction::Pause,
                    elapsed_ms: 65_432,
                },
                TimerLogEntry {
                    id: "1722222290000-3".to_string(),
                    timestamp: 1_722_222_290_000,
                    action: TimerAction::Resume,
                    elapsed_ms: 65_432,
                },
            ],
            digit_opacity: 0.55,
        }
    }

    #[test]
    fn load_uses_workspace_tools_directory() {
        let root = TempDir::new().unwrap();
        let store = TimerStore::load(root.path()).unwrap();
        let expected = timer_path(root.path());

        assert_eq!(store.path, expected);
        assert_eq!(store.get(), TimerData::default());
        store.save(TimerData::default()).unwrap();
        assert!(expected.is_file());
        assert!(!root.path().join("timer.json").exists());
    }

    #[test]
    fn saves_and_reloads_timer_data() {
        let root = TempDir::new().unwrap();
        let store = TimerStore::load(root.path()).unwrap();
        let data = valid_data();

        assert_eq!(store.save(data.clone()).unwrap(), data);
        assert_eq!(store.get(), data);
        assert_eq!(TimerStore::load(root.path()).unwrap().get(), data);

        let persisted = serde_json::from_slice::<serde_json::Value>(
            &fs::read(timer_path(root.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["accumulatedMs"], 65_432);
        assert_eq!(persisted["runningSince"], 1_722_222_222_333_u64);
        assert_eq!(persisted["digitOpacity"], 0.55);
        assert_eq!(persisted["logs"][0]["elapsedMs"], 0);
    }

    #[test]
    fn accepts_boundary_opacities_and_exact_log_limit() {
        let root = TempDir::new().unwrap();
        let store = TimerStore::load(root.path()).unwrap();

        for opacity in [0.0, 1.0] {
            let mut data = valid_data();
            data.digit_opacity = opacity;
            assert_eq!(store.save(data.clone()).unwrap(), data);
        }

        let data = TimerData {
            logs: (0..TIMER_LOG_LIMIT)
                .map(|index| TimerLogEntry {
                    id: format!("log-{index}"),
                    timestamp: index as u64,
                    action: TimerAction::Reset,
                    elapsed_ms: index as u64,
                })
                .collect(),
            ..TimerData::default()
        };
        assert!(store.save(data).is_ok());
    }

    #[test]
    fn rejects_invalid_values_without_changing_saved_data() {
        let root = TempDir::new().unwrap();
        let store = TimerStore::load(root.path()).unwrap();
        let original = valid_data();
        store.save(original.clone()).unwrap();

        let mut invalid_values = Vec::new();

        let mut wrong_version = original.clone();
        wrong_version.version = TIMER_DATA_VERSION + 1;
        invalid_values.push(wrong_version);

        for opacity in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
            let mut invalid = original.clone();
            invalid.digit_opacity = opacity;
            invalid_values.push(invalid);
        }

        let mut accumulated_too_large = original.clone();
        accumulated_too_large.accumulated_ms = MAX_SAFE_INTEGER + 1;
        invalid_values.push(accumulated_too_large);

        let mut running_since_too_large = original.clone();
        running_since_too_large.running_since = Some(MAX_SAFE_INTEGER + 1);
        invalid_values.push(running_since_too_large);

        for invalid in invalid_values {
            assert_eq!(store.save(invalid).unwrap_err().code, "invalid_input");
            assert_eq!(store.get(), original);
        }
        assert_eq!(TimerStore::load(root.path()).unwrap().get(), original);
    }

    #[test]
    fn rejects_invalid_record_ids_numbers_and_log_overflow() {
        let root = TempDir::new().unwrap();
        let store = TimerStore::load(root.path()).unwrap();

        let mut empty_id = valid_data();
        empty_id.logs[0].id.clear();
        assert_eq!(store.save(empty_id).unwrap_err().code, "invalid_input");

        let mut whitespace_id = valid_data();
        whitespace_id.logs[0].id = " log-1 ".to_string();
        assert_eq!(store.save(whitespace_id).unwrap_err().code, "invalid_input");

        let mut control_id = valid_data();
        control_id.logs[0].id = "bad\nid".to_string();
        assert_eq!(store.save(control_id).unwrap_err().code, "invalid_input");

        let mut long_id = valid_data();
        long_id.logs[0].id = "x".repeat(MAX_LOG_ID_CHARS + 1);
        assert_eq!(store.save(long_id).unwrap_err().code, "invalid_input");

        let mut duplicate_id = valid_data();
        duplicate_id.logs[1].id = duplicate_id.logs[0].id.clone();
        assert_eq!(store.save(duplicate_id).unwrap_err().code, "invalid_input");

        let mut timestamp_too_large = valid_data();
        timestamp_too_large.logs[0].timestamp = MAX_SAFE_INTEGER + 1;
        assert_eq!(
            store.save(timestamp_too_large).unwrap_err().code,
            "invalid_input"
        );

        let mut elapsed_too_large = valid_data();
        elapsed_too_large.logs[0].elapsed_ms = MAX_SAFE_INTEGER + 1;
        assert_eq!(
            store.save(elapsed_too_large).unwrap_err().code,
            "invalid_input"
        );

        let too_many = TimerData {
            logs: (0..=TIMER_LOG_LIMIT)
                .map(|index| TimerLogEntry {
                    id: format!("log-{index}"),
                    timestamp: index as u64,
                    action: TimerAction::Reset,
                    elapsed_ms: index as u64,
                })
                .collect(),
            ..TimerData::default()
        };
        assert_eq!(store.save(too_many).unwrap_err().code, "invalid_input");
    }

    #[test]
    fn backs_up_malformed_json_and_replaces_it_with_defaults() {
        let root = TempDir::new().unwrap();
        let path = timer_path(root.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{not valid json").unwrap();

        let store = TimerStore::load(root.path()).unwrap();

        assert_eq!(store.get(), TimerData::default());
        let persisted = serde_json::from_slice::<TimerData>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted, TimerData::default());
        let backups = corrupt_backups(path.parent().unwrap());
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), b"{not valid json");
    }

    #[test]
    fn backs_up_semantically_invalid_data_and_replaces_it_with_defaults() {
        let root = TempDir::new().unwrap();
        let path = timer_path(root.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let invalid = json!({
            "version": 1,
            "accumulatedMs": 0,
            "runningSince": null,
            "logs": [{
                "id": "log-1",
                "timestamp": 1,
                "action": "pause",
                "elapsedMs": 0
            }],
            "digitOpacity": 1.5
        });
        let original = serde_json::to_vec_pretty(&invalid).unwrap();
        fs::write(&path, &original).unwrap();

        assert_eq!(
            TimerStore::load(root.path()).unwrap().get(),
            TimerData::default()
        );
        let backups = corrupt_backups(path.parent().unwrap());
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), original);
    }

    #[test]
    fn loads_legacy_state_without_digit_opacity_using_the_default() {
        let root = TempDir::new().unwrap();
        let path = timer_path(root.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "accumulatedMs": 12_345,
                "runningSince": null,
                "logs": []
            }))
            .unwrap(),
        )
        .unwrap();

        let data = TimerStore::load(root.path()).unwrap().get();
        assert_eq!(data.accumulated_ms, 12_345);
        assert_eq!(data.digit_opacity, 1.0);
        assert!(corrupt_backups(path.parent().unwrap()).is_empty());
    }

    fn corrupt_backups(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with("timer.corrupt-") && name.ends_with(".json")
                })
            })
            .collect()
    }
}
