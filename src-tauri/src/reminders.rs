use crate::error::{AppError, AppResult};
use crate::storage::{atomic_write_json, INTERNAL_DATA_DIR};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Timelike, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

const MIN_INTERVAL_SECONDS: u64 = 60;
const LOCAL_DATETIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReminderKind {
    Once,
    Interval,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReminderSchedule {
    pub kind: ReminderKind,
    pub anchor_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub id: String,
    pub title: String,
    pub message: String,
    pub schedule: ReminderSchedule,
    pub enabled: bool,
    pub next_due_at: Option<String>,
    pub last_triggered_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpsertReminderRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub message: String,
    pub schedule: ReminderSchedule,
    pub enabled: bool,
}

pub struct ReminderStore {
    path: PathBuf,
    reminders: Mutex<Vec<Reminder>>,
}

#[derive(Debug, Clone)]
struct DueReminder {
    reminder: Reminder,
    due_at: String,
}

impl ReminderStore {
    pub fn load(root: &Path) -> AppResult<Self> {
        Self::load_from(
            &root
                .join(INTERNAL_DATA_DIR)
                .join("tools")
                .join("reminders.json"),
        )
    }

    fn load_from(path: &Path) -> AppResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| AppError::io("创建提醒数据目录", error))?;
        }
        let reminders = match fs::read(path) {
            Ok(bytes) => match decode_reminders(&bytes, Local::now().naive_local()) {
                Ok((reminders, false)) => reminders,
                Ok((reminders, true)) => {
                    preserve_and_replace_corrupt_file(path, &reminders);
                    reminders
                }
                Err(_) => {
                    let reminders = Vec::new();
                    preserve_and_replace_corrupt_file(path, &reminders);
                    reminders
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(AppError::io("读取提醒数据", error)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            reminders: Mutex::new(reminders),
        })
    }

    #[cfg(test)]
    fn for_test(app_data: &Path) -> AppResult<Self> {
        Self::load_from(&app_data.join("reminders.json"))
    }

    pub fn list(&self) -> Vec<Reminder> {
        let mut reminders = self
            .reminders
            .lock()
            .expect("reminder store lock poisoned")
            .clone();
        reminders.sort_by(|left, right| {
            right
                .enabled
                .cmp(&left.enabled)
                .then_with(|| left.next_due_at.cmp(&right.next_due_at))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        reminders
    }

    pub fn upsert(&self, request: UpsertReminderRequest) -> AppResult<Reminder> {
        self.upsert_at(request, Local::now().naive_local())
    }

    fn upsert_at(&self, request: UpsertReminderRequest, now: NaiveDateTime) -> AppResult<Reminder> {
        let title = request.title.trim();
        if title.is_empty() {
            return Err(AppError::invalid("提醒标题不能为空"));
        }
        let (schedule, _) = normalize_schedule(request.schedule)?;
        let next_due_at = next_occurrence_after(&schedule, now)?
            .map(|date_time| format_local_datetime(&date_time));
        if request.enabled && matches!(schedule.kind, ReminderKind::Once) && next_due_at.is_none() {
            return Err(AppError::invalid("一次性提醒时间必须晚于当前时间"));
        }

        let mut current = self.reminders.lock().expect("reminder store lock poisoned");
        let mut reminders = current.clone();
        let now_text = format_local_datetime(&now);
        let reminder = if let Some(id) = request.id {
            validate_reminder_id(&id)?;
            let existing = reminders
                .iter_mut()
                .find(|reminder| reminder.id == id)
                .ok_or_else(|| AppError::not_found("没有找到这个提醒"))?;
            existing.title = title.to_string();
            existing.message = request.message.trim().to_string();
            existing.schedule = schedule;
            existing.enabled = request.enabled;
            existing.next_due_at = next_due_at;
            existing.updated_at = now_text;
            existing.clone()
        } else {
            let reminder = Reminder {
                id: Uuid::new_v4().to_string(),
                title: title.to_string(),
                message: request.message.trim().to_string(),
                schedule,
                enabled: request.enabled,
                next_due_at,
                last_triggered_at: None,
                created_at: now_text.clone(),
                updated_at: now_text,
            };
            reminders.push(reminder.clone());
            reminder
        };
        self.persist(&reminders)?;
        *current = reminders;
        Ok(reminder)
    }

    pub fn delete(&self, reminder_id: &str) -> AppResult<()> {
        validate_reminder_id(reminder_id)?;
        let mut current = self.reminders.lock().expect("reminder store lock poisoned");
        let mut reminders = current.clone();
        let original_len = reminders.len();
        reminders.retain(|reminder| reminder.id != reminder_id);
        if reminders.len() == original_len {
            return Err(AppError::not_found("没有找到这个提醒"));
        }
        self.persist(&reminders)?;
        *current = reminders;
        Ok(())
    }

    pub fn set_enabled(&self, reminder_id: &str, enabled: bool) -> AppResult<Reminder> {
        self.set_enabled_at(reminder_id, enabled, Local::now().naive_local())
    }

    fn set_enabled_at(
        &self,
        reminder_id: &str,
        enabled: bool,
        now: NaiveDateTime,
    ) -> AppResult<Reminder> {
        validate_reminder_id(reminder_id)?;
        let mut current = self.reminders.lock().expect("reminder store lock poisoned");
        let mut reminders = current.clone();
        let reminder = reminders
            .iter_mut()
            .find(|reminder| reminder.id == reminder_id)
            .ok_or_else(|| AppError::not_found("没有找到这个提醒"))?;
        if enabled {
            reminder.next_due_at = next_occurrence_after(&reminder.schedule, now)?
                .map(|date_time| format_local_datetime(&date_time));
            if reminder.next_due_at.is_none() {
                return Err(AppError::invalid("提醒时间已过，请先修改提醒时间"));
            }
        }
        reminder.enabled = enabled;
        reminder.updated_at = format_local_datetime(&now);
        let result = reminder.clone();
        self.persist(&reminders)?;
        *current = reminders;
        Ok(result)
    }

    fn pending_due(&self) -> AppResult<Vec<DueReminder>> {
        self.pending_due_at(Local::now().naive_local())
    }

    fn pending_due_at(&self, now: NaiveDateTime) -> AppResult<Vec<DueReminder>> {
        let mut current = self.reminders.lock().expect("reminder store lock poisoned");
        let mut reminders = current.clone();
        let mut due = Vec::new();
        let now_text = format_local_datetime(&now);

        for reminder in reminders.iter_mut().filter(|reminder| reminder.enabled) {
            let normalized_schedule = match normalize_schedule(reminder.schedule.clone()) {
                Ok((schedule, _)) => schedule,
                Err(_) => {
                    disable_invalid_reminder(reminder, &now_text);
                    continue;
                }
            };
            reminder.schedule = normalized_schedule;

            let due_at = match reminder.next_due_at.as_deref().map(parse_local_datetime) {
                Some(Ok(due_at)) => due_at,
                Some(Err(_)) | None => match recover_next_due(reminder) {
                    Ok(due_at) => {
                        reminder.next_due_at = Some(format_local_datetime(&due_at));
                        due_at
                    }
                    Err(_) => {
                        disable_invalid_reminder(reminder, &now_text);
                        continue;
                    }
                },
            };
            let due_at_text = format_local_datetime(&due_at);
            if reminder.next_due_at.as_deref() != Some(due_at_text.as_str()) {
                reminder.next_due_at = Some(due_at_text.clone());
            }
            if due_at > now {
                continue;
            }

            due.push(DueReminder {
                reminder: reminder.clone(),
                due_at: due_at_text,
            });
        }

        if reminders != *current {
            self.persist(&reminders)?;
            *current = reminders;
        }
        Ok(due)
    }

    fn acknowledge_due(&self, due: &DueReminder) -> AppResult<Option<Reminder>> {
        self.acknowledge_due_at(due, Local::now().naive_local())
    }

    fn is_due_current(&self, due: &DueReminder) -> bool {
        self.reminders
            .lock()
            .expect("reminder store lock poisoned")
            .iter()
            .any(|reminder| reminder == &due.reminder)
    }

    fn acknowledge_due_at(
        &self,
        due: &DueReminder,
        now: NaiveDateTime,
    ) -> AppResult<Option<Reminder>> {
        let mut current = self.reminders.lock().expect("reminder store lock poisoned");
        let mut reminders = current.clone();
        let Some(reminder) = reminders
            .iter_mut()
            .find(|reminder| reminder.id == due.reminder.id)
        else {
            return Ok(None);
        };
        if !reminder.enabled
            || reminder.next_due_at.as_deref() != Some(due.due_at.as_str())
            || reminder.schedule != due.reminder.schedule
        {
            return Ok(None);
        }

        reminder.last_triggered_at = Some(format_local_datetime(&now));
        if matches!(reminder.schedule.kind, ReminderKind::Once) {
            reminder.enabled = false;
            reminder.next_due_at = None;
        } else {
            match next_occurrence_after(&reminder.schedule, now) {
                Ok(Some(next_due_at)) => {
                    reminder.next_due_at = Some(format_local_datetime(&next_due_at));
                }
                Ok(None) | Err(_) => {
                    reminder.enabled = false;
                    reminder.next_due_at = None;
                }
            }
        }
        reminder.updated_at = format_local_datetime(&now);
        let triggered = reminder.clone();
        self.persist(&reminders)?;
        *current = reminders;
        Ok(Some(triggered))
    }

    #[cfg(test)]
    fn take_due_at(&self, now: NaiveDateTime) -> AppResult<Vec<Reminder>> {
        let due = self.pending_due_at(now)?;
        let mut triggered = Vec::with_capacity(due.len());
        for reminder in due {
            if let Some(reminder) = self.acknowledge_due_at(&reminder, now)? {
                triggered.push(reminder);
            }
        }
        Ok(triggered)
    }

    fn persist(&self, reminders: &[Reminder]) -> AppResult<()> {
        atomic_write_json(&self.path, reminders)
    }
}

#[tauri::command]
pub fn list_reminders(store: State<'_, ReminderStore>) -> Vec<Reminder> {
    store.list()
}

#[tauri::command]
pub fn upsert_reminder(
    app: AppHandle,
    store: State<'_, ReminderStore>,
    request: UpsertReminderRequest,
) -> AppResult<Reminder> {
    let reminder = store.upsert(request)?;
    let _ = app.emit(
        "reminder_changed",
        json!({ "id": reminder.id, "kind": "upserted", "reminder": reminder }),
    );
    Ok(reminder)
}

#[tauri::command]
pub fn delete_reminder(
    app: AppHandle,
    store: State<'_, ReminderStore>,
    reminder_id: String,
) -> AppResult<()> {
    store.delete(&reminder_id)?;
    let _ = app.emit(
        "reminder_changed",
        json!({ "id": reminder_id, "kind": "deleted" }),
    );
    Ok(())
}

#[tauri::command]
pub fn set_reminder_enabled(
    app: AppHandle,
    store: State<'_, ReminderStore>,
    reminder_id: String,
    enabled: bool,
) -> AppResult<Reminder> {
    let reminder = store.set_enabled(&reminder_id, enabled)?;
    let _ = app.emit(
        "reminder_changed",
        json!({ "id": reminder.id, "kind": "enabled", "reminder": reminder }),
    );
    Ok(reminder)
}

pub fn start_scheduler(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let store = app.state::<ReminderStore>();
        let Ok(reminders) = store.pending_due() else {
            continue;
        };
        for due in reminders {
            if !store.is_due_current(&due) {
                continue;
            }
            let body = if due.reminder.message.trim().is_empty() {
                "提醒时间到了"
            } else {
                &due.reminder.message
            };
            if app
                .notification()
                .builder()
                .title(format!("飞花 - PetalDesk 提醒 · {}", due.reminder.title))
                .body(body)
                .show()
                .is_err()
            {
                continue;
            }
            let Ok(Some(reminder)) = store.acknowledge_due(&due) else {
                continue;
            };
            let _ = app.emit(
                "reminder_changed",
                json!({ "id": reminder.id, "kind": "triggered", "reminder": reminder }),
            );
        }
    });
}

fn decode_reminders(
    bytes: &[u8],
    now: NaiveDateTime,
) -> Result<(Vec<Reminder>, bool), serde_json::Error> {
    let values = serde_json::from_slice::<Vec<serde_json::Value>>(bytes)?;
    let mut reminders = Vec::with_capacity(values.len());
    let mut seen_ids = HashSet::with_capacity(values.len());
    let mut recovered = false;

    for value in values {
        let Ok(mut reminder) = serde_json::from_value::<Reminder>(value) else {
            recovered = true;
            continue;
        };
        match normalize_loaded_reminder(&mut reminder, now) {
            Ok(changed) => {
                if seen_ids.insert(reminder.id.clone()) {
                    recovered |= changed;
                    reminders.push(reminder);
                } else {
                    recovered = true;
                }
            }
            Err(_) => recovered = true,
        }
    }

    Ok((reminders, recovered))
}

fn normalize_loaded_reminder(reminder: &mut Reminder, now: NaiveDateTime) -> AppResult<bool> {
    validate_reminder_id(&reminder.id)?;
    if reminder.title.trim().is_empty() {
        return Err(AppError::invalid("提醒标题不能为空"));
    }

    let mut changed = false;
    let (schedule, _) = normalize_schedule(reminder.schedule.clone())?;
    if reminder.schedule != schedule {
        reminder.schedule = schedule;
        changed = true;
    }

    if let Some(last_triggered_at) = reminder.last_triggered_at.as_deref() {
        match parse_local_datetime(last_triggered_at) {
            Ok(last_triggered_at) => {
                let normalized = format_local_datetime(&last_triggered_at);
                if reminder.last_triggered_at.as_deref() != Some(normalized.as_str()) {
                    reminder.last_triggered_at = Some(normalized);
                    changed = true;
                }
            }
            Err(_) => {
                reminder.last_triggered_at = None;
                changed = true;
            }
        }
    }

    match reminder.next_due_at.as_deref().map(parse_local_datetime) {
        Some(Ok(next_due_at)) => {
            let normalized = format_local_datetime(&next_due_at);
            if reminder.next_due_at.as_deref() != Some(normalized.as_str()) {
                reminder.next_due_at = Some(normalized);
                changed = true;
            }
        }
        Some(Err(_)) | None if reminder.enabled => {
            if matches!(reminder.schedule.kind, ReminderKind::Once)
                && reminder.last_triggered_at.is_some()
            {
                reminder.enabled = false;
                reminder.next_due_at = None;
            } else {
                reminder.next_due_at = Some(format_local_datetime(&recover_next_due(reminder)?));
            }
            reminder.updated_at = format_local_datetime(&now);
            changed = true;
        }
        Some(Err(_)) => {
            reminder.next_due_at = None;
            changed = true;
        }
        None => {}
    }

    Ok(changed)
}

fn recover_next_due(reminder: &mut Reminder) -> AppResult<NaiveDateTime> {
    let (schedule, anchor) = normalize_schedule(reminder.schedule.clone())?;
    reminder.schedule = schedule;
    if matches!(reminder.schedule.kind, ReminderKind::Once) {
        return Ok(anchor);
    }
    if let Some(last_triggered_at) = reminder.last_triggered_at.as_deref() {
        if let Ok(last_triggered_at) = parse_local_datetime(last_triggered_at) {
            if let Some(next_due_at) = next_occurrence_after(&reminder.schedule, last_triggered_at)?
            {
                return Ok(next_due_at);
            }
        } else {
            reminder.last_triggered_at = None;
        }
    }
    Ok(anchor)
}

fn disable_invalid_reminder(reminder: &mut Reminder, now: &str) {
    reminder.enabled = false;
    reminder.next_due_at = None;
    reminder.updated_at = now.to_string();
}

fn normalize_schedule(
    mut schedule: ReminderSchedule,
) -> AppResult<(ReminderSchedule, NaiveDateTime)> {
    let anchor = parse_local_datetime(&schedule.anchor_at)?;
    schedule.anchor_at = format_local_datetime(&anchor);
    match schedule.kind {
        ReminderKind::Interval => {
            let interval = schedule
                .interval_seconds
                .ok_or_else(|| AppError::invalid("间隔提醒必须设置间隔时长"))?;
            if interval < MIN_INTERVAL_SECONDS {
                return Err(AppError::invalid("提醒间隔不能少于 60 秒"));
            }
            i64::try_from(interval).map_err(|_| AppError::invalid("提醒间隔过大"))?;
        }
        _ => schedule.interval_seconds = None,
    }
    Ok((schedule, anchor))
}

fn next_occurrence_after(
    schedule: &ReminderSchedule,
    after: NaiveDateTime,
) -> AppResult<Option<NaiveDateTime>> {
    let (schedule, anchor) = normalize_schedule(schedule.clone())?;
    if after < anchor {
        return Ok(Some(anchor));
    }

    let next = match schedule.kind {
        ReminderKind::Once => None,
        ReminderKind::Interval => {
            let interval = i64::try_from(schedule.interval_seconds.expect("validated interval"))
                .map_err(|_| AppError::invalid("提醒间隔过大"))?;
            let elapsed = after.signed_duration_since(anchor).num_seconds();
            let steps = elapsed
                .checked_div(interval)
                .and_then(|steps| steps.checked_add(1))
                .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?;
            let seconds = interval
                .checked_mul(steps)
                .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?;
            anchor.checked_add_signed(Duration::seconds(seconds))
        }
        ReminderKind::Daily => {
            let mut candidate = after.date().and_time(anchor.time());
            if candidate <= after {
                candidate = candidate
                    .checked_add_signed(Duration::days(1))
                    .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?;
            }
            Some(candidate)
        }
        ReminderKind::Weekly => {
            let days_ahead = weekday_distance(after.weekday(), anchor.weekday());
            let mut candidate = after
                .date()
                .checked_add_signed(Duration::days(i64::from(days_ahead)))
                .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?
                .and_time(anchor.time());
            if candidate <= after {
                candidate = candidate
                    .checked_add_signed(Duration::days(7))
                    .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?;
            }
            Some(candidate)
        }
        ReminderKind::Monthly => {
            let mut year = after.year();
            let mut month = after.month();
            let mut candidate = calendar_candidate(year, month, anchor.day(), anchor)?;
            if candidate <= after {
                (year, month) = following_month(year, month)?;
                candidate = calendar_candidate(year, month, anchor.day(), anchor)?;
            }
            Some(candidate)
        }
        ReminderKind::Yearly => {
            let mut year = after.year();
            let mut candidate = calendar_candidate(year, anchor.month(), anchor.day(), anchor)?;
            if candidate <= after {
                year = year
                    .checked_add(1)
                    .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?;
                candidate = calendar_candidate(year, anchor.month(), anchor.day(), anchor)?;
            }
            Some(candidate)
        }
    };
    Ok(next)
}

fn parse_local_datetime(value: &str) -> AppResult<NaiveDateTime> {
    let value = value.trim();
    ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"]
        .iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
        .ok_or_else(|| AppError::invalid("提醒时间格式无效"))
}

fn format_local_datetime(value: &NaiveDateTime) -> String {
    value.format(LOCAL_DATETIME_FORMAT).to_string()
}

fn weekday_distance(from: Weekday, to: Weekday) -> u32 {
    (to.num_days_from_monday() + 7 - from.num_days_from_monday()) % 7
}

fn following_month(year: i32, month: u32) -> AppResult<(i32, u32)> {
    if month == 12 {
        Ok((
            year.checked_add(1)
                .ok_or_else(|| AppError::invalid("无法计算下次提醒时间"))?,
            1,
        ))
    } else {
        Ok((year, month + 1))
    }
}

fn days_in_month(year: i32, month: u32) -> AppResult<u32> {
    let (next_year, next_month) = following_month(year, month)?;
    let first_of_next_month = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .ok_or_else(|| AppError::invalid("提醒日期超出支持范围"))?;
    first_of_next_month
        .pred_opt()
        .map(|date| date.day())
        .ok_or_else(|| AppError::invalid("提醒日期超出支持范围"))
}

fn calendar_candidate(
    year: i32,
    month: u32,
    preferred_day: u32,
    anchor: NaiveDateTime,
) -> AppResult<NaiveDateTime> {
    let day = preferred_day.min(days_in_month(year, month)?);
    NaiveDate::from_ymd_opt(year, month, day)
        .map(|date| {
            date.and_hms_opt(anchor.hour(), anchor.minute(), anchor.second())
                .expect("anchor time is valid")
        })
        .ok_or_else(|| AppError::invalid("提醒日期超出支持范围"))
}

fn validate_reminder_id(id: &str) -> AppResult<()> {
    let parsed = Uuid::parse_str(id).map_err(|_| AppError::invalid("提醒 ID 无效"))?;
    if parsed.to_string() != id.to_ascii_lowercase() {
        return Err(AppError::invalid("提醒 ID 格式无效"));
    }
    Ok(())
}

fn preserve_corrupt_file(path: &Path) -> AppResult<()> {
    let Some(parent) = path.parent() else {
        return Err(AppError::invalid("提醒数据文件没有父目录"));
    };
    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    let backup = parent.join(format!(
        "reminders.corrupt-{timestamp}-{}.json",
        Uuid::new_v4()
    ));
    if fs::rename(path, &backup).is_ok() {
        return Ok(());
    }
    fs::copy(path, &backup)
        .map(|_| ())
        .map_err(|error| AppError::io("备份损坏的提醒数据", error))
}

fn preserve_and_replace_corrupt_file(path: &Path, reminders: &[Reminder]) {
    if preserve_corrupt_file(path).is_ok() {
        let _ = atomic_write_json(path, reminders);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_uses_workspace_tools_directory() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::load(root.path()).unwrap();
        let expected = root
            .path()
            .join(INTERNAL_DATA_DIR)
            .join("tools")
            .join("reminders.json");

        assert_eq!(store.path, expected);
        store.persist(&[]).unwrap();
        assert!(expected.is_file());
        assert!(!root.path().join("reminders.json").exists());
    }

    fn date_time(value: &str) -> NaiveDateTime {
        parse_local_datetime(value).unwrap()
    }

    fn schedule(kind: ReminderKind, anchor_at: &str) -> ReminderSchedule {
        ReminderSchedule {
            kind,
            anchor_at: anchor_at.to_string(),
            interval_seconds: None,
        }
    }

    fn request(kind: ReminderKind, anchor_at: &str) -> UpsertReminderRequest {
        UpsertReminderRequest {
            id: None,
            title: "测试提醒".to_string(),
            message: "该休息一下了".to_string(),
            schedule: schedule(kind, anchor_at),
            enabled: true,
        }
    }

    #[test]
    fn computes_once_interval_daily_and_weekly_branches() {
        let after = date_time("2026-07-12T10:00:00");
        assert_eq!(
            next_occurrence_after(&schedule(ReminderKind::Once, "2026-07-12T10:00:01"), after)
                .unwrap(),
            Some(date_time("2026-07-12T10:00:01"))
        );
        assert_eq!(
            next_occurrence_after(&schedule(ReminderKind::Once, "2026-07-12T09:59:59"), after)
                .unwrap(),
            None
        );

        let mut interval = schedule(ReminderKind::Interval, "2026-07-12T09:55:00");
        interval.interval_seconds = Some(120);
        assert_eq!(
            next_occurrence_after(&interval, after).unwrap(),
            Some(date_time("2026-07-12T10:01:00"))
        );
        assert_eq!(
            next_occurrence_after(&schedule(ReminderKind::Daily, "2026-01-01T09:30:15"), after)
                .unwrap(),
            Some(date_time("2026-07-13T09:30:15"))
        );
        assert_eq!(
            next_occurrence_after(
                &schedule(ReminderKind::Weekly, "2026-07-06T11:00:00"),
                after
            )
            .unwrap(),
            Some(date_time("2026-07-13T11:00:00"))
        );
    }

    #[test]
    fn clamps_month_end_and_leap_day_without_losing_the_anchor_day() {
        let monthly = schedule(ReminderKind::Monthly, "2026-01-31T08:15:00");
        assert_eq!(
            next_occurrence_after(&monthly, date_time("2026-02-01T00:00:00")).unwrap(),
            Some(date_time("2026-02-28T08:15:00"))
        );
        assert_eq!(
            next_occurrence_after(&monthly, date_time("2026-02-28T09:00:00")).unwrap(),
            Some(date_time("2026-03-31T08:15:00"))
        );

        let yearly = schedule(ReminderKind::Yearly, "2024-02-29T17:45:00");
        assert_eq!(
            next_occurrence_after(&yearly, date_time("2026-03-01T00:00:00")).unwrap(),
            Some(date_time("2027-02-28T17:45:00"))
        );
        assert_eq!(
            next_occurrence_after(&yearly, date_time("2027-03-01T00:00:00")).unwrap(),
            Some(date_time("2028-02-29T17:45:00"))
        );
    }

    #[test]
    fn validates_title_once_time_and_minimum_interval() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::for_test(root.path()).unwrap();
        let now = date_time("2026-07-12T10:00:00");

        let mut empty = request(ReminderKind::Once, "2026-07-12T10:01:00");
        empty.title = "  ".to_string();
        assert_eq!(
            store.upsert_at(empty, now).unwrap_err().code,
            "invalid_input"
        );

        let past = request(ReminderKind::Once, "2026-07-12T09:59:59");
        assert_eq!(
            store.upsert_at(past, now).unwrap_err().code,
            "invalid_input"
        );

        let mut disabled_past = request(ReminderKind::Once, "2026-07-12T09:59:59");
        disabled_past.enabled = false;
        let disabled_past = store.upsert_at(disabled_past, now).unwrap();
        assert!(!disabled_past.enabled);
        assert_eq!(disabled_past.next_due_at, None);

        let mut too_short = request(ReminderKind::Interval, "2026-07-12T10:01:00");
        too_short.schedule.interval_seconds = Some(59);
        assert_eq!(
            store.upsert_at(too_short, now).unwrap_err().code,
            "invalid_input"
        );
    }

    #[test]
    fn persists_crud_and_recomputes_when_enabled() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::for_test(root.path()).unwrap();
        let now = date_time("2026-07-12T10:00:00");
        let mut create = request(ReminderKind::Daily, "2026-07-12T11:00:00");
        create.enabled = false;
        let created = store.upsert_at(create, now).unwrap();
        assert!(!created.enabled);
        assert_eq!(created.next_due_at.as_deref(), Some("2026-07-12T11:00:00"));

        let reloaded = ReminderStore::for_test(root.path()).unwrap();
        assert_eq!(reloaded.list(), vec![created.clone()]);
        let enabled = reloaded
            .set_enabled_at(&created.id, true, date_time("2026-07-12T12:00:00"))
            .unwrap();
        assert!(enabled.enabled);
        assert_eq!(enabled.next_due_at.as_deref(), Some("2026-07-13T11:00:00"));

        let mut update = request(ReminderKind::Weekly, "2026-07-13T08:00:00");
        update.id = Some(created.id.clone());
        update.title = "修改后的提醒".to_string();
        let updated = reloaded
            .upsert_at(update, date_time("2026-07-12T12:00:01"))
            .unwrap();
        assert_eq!(updated.created_at, created.created_at);
        assert_eq!(updated.title, "修改后的提醒");

        reloaded.delete(&created.id).unwrap();
        assert!(ReminderStore::for_test(root.path())
            .unwrap()
            .list()
            .is_empty());
    }

    #[test]
    fn overdue_recurrence_triggers_once_and_advances_into_the_future() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::for_test(root.path()).unwrap();
        let now = date_time("2026-07-12T10:00:00");
        let id = Uuid::new_v4().to_string();
        let reminder = Reminder {
            id: id.clone(),
            title: "补发提醒".to_string(),
            message: String::new(),
            schedule: ReminderSchedule {
                kind: ReminderKind::Interval,
                anchor_at: "2026-07-12T09:00:00".to_string(),
                interval_seconds: Some(300),
            },
            enabled: true,
            next_due_at: Some("2026-07-12T09:05:00".to_string()),
            last_triggered_at: None,
            created_at: "2026-07-12T09:00:00".to_string(),
            updated_at: "2026-07-12T09:00:00".to_string(),
        };
        store.persist(std::slice::from_ref(&reminder)).unwrap();
        *store
            .reminders
            .lock()
            .expect("reminder store lock poisoned") = vec![reminder];

        let triggered = store.take_due_at(now).unwrap();
        assert_eq!(triggered.len(), 1);
        assert_eq!(
            triggered[0].last_triggered_at.as_deref(),
            Some("2026-07-12T10:00:00")
        );
        assert_eq!(
            triggered[0].next_due_at.as_deref(),
            Some("2026-07-12T10:05:00")
        );
        assert!(store
            .take_due_at(date_time("2026-07-12T10:00:01"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn due_once_is_disabled_after_the_notification_is_claimed() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::for_test(root.path()).unwrap();
        let id = Uuid::new_v4().to_string();
        let reminder = Reminder {
            id,
            title: "一次提醒".to_string(),
            message: "到点".to_string(),
            schedule: schedule(ReminderKind::Once, "2026-07-12T10:00:00"),
            enabled: true,
            next_due_at: Some("2026-07-12T10:00:00".to_string()),
            last_triggered_at: None,
            created_at: "2026-07-12T09:00:00".to_string(),
            updated_at: "2026-07-12T09:00:00".to_string(),
        };
        store.persist(std::slice::from_ref(&reminder)).unwrap();
        *store
            .reminders
            .lock()
            .expect("reminder store lock poisoned") = vec![reminder];

        let triggered = store.take_due_at(date_time("2026-07-12T10:00:05")).unwrap();
        assert_eq!(triggered.len(), 1);
        assert!(!triggered[0].enabled);
        assert_eq!(triggered[0].next_due_at, None);
        assert!(store
            .take_due_at(date_time("2026-07-12T10:00:06"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn preserves_corrupt_json_and_starts_with_an_empty_store() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("reminders.json");
        fs::write(&path, b"{not valid json").unwrap();

        let store = ReminderStore::for_test(root.path()).unwrap();
        assert!(store.list().is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"[]");
        let backups = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("reminders.corrupt-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), b"{not valid json");
    }

    #[test]
    fn recovers_valid_entries_when_only_part_of_the_file_is_corrupt() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("reminders.json");
        let store = ReminderStore::for_test(root.path()).unwrap();
        let valid = store
            .upsert_at(
                request(ReminderKind::Daily, "2026-07-12T11:00:00"),
                date_time("2026-07-12T10:00:00"),
            )
            .unwrap();
        let mixed = json!([valid, { "id": "damaged-entry" }]);
        fs::write(&path, serde_json::to_vec_pretty(&mixed).unwrap()).unwrap();

        let recovered = ReminderStore::for_test(root.path()).unwrap();
        assert_eq!(recovered.list().len(), 1);
        let persisted = serde_json::from_slice::<Vec<Reminder>>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted, recovered.list());
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("reminders.corrupt-"))
                .count(),
            1
        );
    }

    #[test]
    fn a_due_reminder_is_not_advanced_until_delivery_is_acknowledged() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::for_test(root.path()).unwrap();
        let reminder = store
            .upsert_at(
                request(ReminderKind::Once, "2026-07-12T10:00:00"),
                date_time("2026-07-12T09:59:00"),
            )
            .unwrap();

        let first_attempt = store
            .pending_due_at(date_time("2026-07-12T10:00:01"))
            .unwrap();
        let retry = store
            .pending_due_at(date_time("2026-07-12T10:00:02"))
            .unwrap();
        assert_eq!(first_attempt.len(), 1);
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].reminder.id, reminder.id);
        assert!(store.list()[0].enabled);

        let delivered = store
            .acknowledge_due_at(&retry[0], date_time("2026-07-12T10:00:02"))
            .unwrap()
            .unwrap();
        assert!(!delivered.enabled);
        assert!(store
            .pending_due_at(date_time("2026-07-12T10:00:03"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn an_invalid_reminder_is_disabled_without_blocking_other_due_items() {
        let root = TempDir::new().unwrap();
        let store = ReminderStore::for_test(root.path()).unwrap();
        let invalid = Reminder {
            id: Uuid::new_v4().to_string(),
            title: "损坏的提醒".to_string(),
            message: String::new(),
            schedule: ReminderSchedule {
                kind: ReminderKind::Interval,
                anchor_at: "2026-07-12T09:00:00".to_string(),
                interval_seconds: Some(1),
            },
            enabled: true,
            next_due_at: Some("not-a-time".to_string()),
            last_triggered_at: None,
            created_at: "2026-07-12T09:00:00".to_string(),
            updated_at: "2026-07-12T09:00:00".to_string(),
        };
        let valid = Reminder {
            id: Uuid::new_v4().to_string(),
            title: "正常提醒".to_string(),
            message: String::new(),
            schedule: schedule(ReminderKind::Once, "2026-07-12T10:00:00"),
            enabled: true,
            next_due_at: Some("2026-07-12T10:00:00".to_string()),
            last_triggered_at: None,
            created_at: "2026-07-12T09:00:00".to_string(),
            updated_at: "2026-07-12T09:00:00".to_string(),
        };
        *store
            .reminders
            .lock()
            .expect("reminder store lock poisoned") = vec![invalid.clone(), valid.clone()];

        let due = store
            .pending_due_at(date_time("2026-07-12T10:00:01"))
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].reminder.id, valid.id);
        let reminders = store.list();
        let invalid = reminders
            .iter()
            .find(|reminder| reminder.id == invalid.id)
            .unwrap();
        assert!(!invalid.enabled);
        assert_eq!(invalid.next_due_at, None);
    }
}
