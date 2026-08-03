use crate::error::{AppError, AppResult};
use crate::storage::WorkspaceStore;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tokio::sync::Notify;
use uuid::Uuid;

#[cfg(windows)]
use tauri_plugin_updater::{Update, UpdaterExt};

pub(crate) const UPDATE_STATE_EVENT: &str = "updater_state_changed";
pub(crate) const PREPARE_INSTALL_EVENT: &str = "updater_prepare_install";
const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(30);
const PERIODIC_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// First retry delay after a failed automatic cycle; doubles per consecutive
/// failure. Without backoff an outage that fails fast (offline, DNS failure,
/// 5xx manifest) turns the scheduler into a tight retry loop.
const FAILURE_RETRY_BASE_DELAY: Duration = Duration::from_secs(5 * 60);
/// Ceiling for the doubling above, so retries never outpace the normal cycle.
const FAILURE_RETRY_MAX_DELAY: Duration = PERIODIC_CHECK_INTERVAL;
const INSTALL_PREPARATION_TIMEOUT: Duration = Duration::from_secs(8);
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettings {
    pub auto_update: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { auto_update: true }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    Installing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub phase: UpdatePhase,
    pub current_version: String,
    pub available_version: Option<String>,
    pub release_notes: Option<String>,
    pub published_at: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            available_version: None,
            release_notes: None,
            published_at: None,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateOperation {
    Checking,
    Downloading,
    PreparingInstall,
    Installing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomaticAction {
    None,
    Check,
    Download,
}

struct UpdateRuntime {
    state: UpdateState,
    operation: Option<UpdateOperation>,
    postponed_version: Option<String>,
    last_progress_emit: Option<Instant>,
    #[cfg(windows)]
    update: Option<Update>,
    #[cfg(windows)]
    package: Option<Arc<Vec<u8>>>,
}

impl Default for UpdateRuntime {
    fn default() -> Self {
        Self {
            state: UpdateState::default(),
            operation: None,
            postponed_version: None,
            last_progress_emit: None,
            #[cfg(windows)]
            update: None,
            #[cfg(windows)]
            package: None,
        }
    }
}

struct InstallPreparation {
    request_id: String,
    pending_windows: HashSet<String>,
    failure: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallPreparationEvent {
    request_id: String,
}

enum PreparationStatus {
    Pending(Vec<String>),
    Complete,
    Failed(String),
    Replaced,
}

pub struct UpdaterManager {
    runtime: Mutex<UpdateRuntime>,
    preparation: Mutex<Option<InstallPreparation>>,
    registered_windows: Mutex<HashSet<String>>,
    preparation_notify: Arc<Notify>,
}

impl Default for UpdaterManager {
    fn default() -> Self {
        Self {
            runtime: Mutex::new(UpdateRuntime::default()),
            preparation: Mutex::new(None),
            registered_windows: Mutex::new(HashSet::new()),
            preparation_notify: Arc::new(Notify::new()),
        }
    }
}

impl UpdaterManager {
    pub fn state(&self) -> UpdateState {
        lock_unpoisoned(&self.runtime).state.clone()
    }

    fn begin_check(&self) -> AppResult<UpdateState> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        ensure_idle_operation(&runtime)?;
        runtime.operation = Some(UpdateOperation::Checking);
        runtime.postponed_version = None;
        runtime.last_progress_emit = None;
        #[cfg(windows)]
        {
            runtime.update = None;
            runtime.package = None;
        }
        runtime.state = UpdateState {
            phase: UpdatePhase::Checking,
            ..UpdateState::default()
        };
        Ok(runtime.state.clone())
    }

    fn finish_up_to_date(&self) -> UpdateState {
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.operation = None;
        runtime.state = UpdateState {
            phase: UpdatePhase::UpToDate,
            ..UpdateState::default()
        };
        runtime.state.clone()
    }

    #[cfg(windows)]
    fn finish_available(&self, update: Update) -> UpdateState {
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.operation = None;
        runtime.state = UpdateState {
            phase: UpdatePhase::Available,
            current_version: update.current_version.clone(),
            available_version: Some(update.version.clone()),
            release_notes: update.body.clone(),
            published_at: update.date.map(|date| date.to_string()),
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
        };
        runtime.update = Some(update);
        runtime.package = None;
        runtime.state.clone()
    }

    fn finish_error(&self, message: impl Into<String>) -> UpdateState {
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.operation = None;
        runtime.state.phase = UpdatePhase::Error;
        runtime.state.error = Some(message.into());
        runtime.last_progress_emit = None;
        runtime.state.clone()
    }

    #[cfg(windows)]
    fn begin_download(&self) -> AppResult<(Option<Update>, UpdateState)> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        ensure_idle_operation(&runtime)?;
        if runtime.package.is_some() {
            runtime.state.phase = UpdatePhase::Ready;
            runtime.state.error = None;
            return Ok((None, runtime.state.clone()));
        }
        let update = runtime
            .update
            .clone()
            .ok_or_else(|| AppError::new("update_not_available", "当前没有可下载的新版本"))?;
        runtime.operation = Some(UpdateOperation::Downloading);
        runtime.state.phase = UpdatePhase::Downloading;
        runtime.state.downloaded_bytes = 0;
        runtime.state.total_bytes = None;
        runtime.state.error = None;
        runtime.last_progress_emit = Some(Instant::now());
        Ok((Some(update), runtime.state.clone()))
    }

    fn record_download_progress(
        &self,
        chunk_bytes: usize,
        total_bytes: Option<u64>,
    ) -> Option<UpdateState> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        if runtime.operation != Some(UpdateOperation::Downloading) {
            return None;
        }
        runtime.state.downloaded_bytes = runtime
            .state
            .downloaded_bytes
            .saturating_add(chunk_bytes as u64);
        if total_bytes.is_some() {
            runtime.state.total_bytes = total_bytes;
        }
        let complete = runtime
            .state
            .total_bytes
            .is_some_and(|total| runtime.state.downloaded_bytes >= total);
        let should_emit = complete
            || runtime
                .last_progress_emit
                .is_none_or(|last| last.elapsed() >= PROGRESS_EMIT_INTERVAL);
        if !should_emit {
            return None;
        }
        runtime.last_progress_emit = Some(Instant::now());
        Some(runtime.state.clone())
    }

    #[cfg(windows)]
    fn finish_download(&self, bytes: Vec<u8>) -> UpdateState {
        let mut runtime = lock_unpoisoned(&self.runtime);
        runtime.operation = None;
        runtime.state.phase = UpdatePhase::Ready;
        runtime.state.downloaded_bytes = bytes.len() as u64;
        if runtime.state.total_bytes.is_none() {
            runtime.state.total_bytes = Some(bytes.len() as u64);
        }
        runtime.state.error = None;
        runtime.last_progress_emit = None;
        runtime.package = Some(Arc::new(bytes));
        runtime.state.clone()
    }

    #[cfg(windows)]
    fn begin_install_preparation(&self) -> AppResult<UpdateState> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        ensure_idle_operation(&runtime)?;
        if runtime.update.is_none() || runtime.package.is_none() {
            return Err(AppError::new("update_not_downloaded", "更新包尚未下载完成"));
        }
        runtime.operation = Some(UpdateOperation::PreparingInstall);
        runtime.state.error = None;
        Ok(runtime.state.clone())
    }

    #[cfg(windows)]
    fn mark_installing(&self) -> AppResult<(Update, Arc<Vec<u8>>, UpdateState)> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        if runtime.operation != Some(UpdateOperation::PreparingInstall) {
            return Err(AppError::new(
                "update_state_changed",
                "更新状态已经发生变化",
            ));
        }
        let update = runtime
            .update
            .clone()
            .ok_or_else(|| AppError::new("update_not_available", "更新任务已经失效"))?;
        let package = runtime
            .package
            .clone()
            .ok_or_else(|| AppError::new("update_not_downloaded", "更新包已经失效"))?;
        runtime.operation = Some(UpdateOperation::Installing);
        runtime.state.phase = UpdatePhase::Installing;
        runtime.state.error = None;
        Ok((update, package, runtime.state.clone()))
    }

    fn abort_install_preparation(&self, message: impl Into<String>) -> UpdateState {
        let mut runtime = lock_unpoisoned(&self.runtime);
        if runtime.operation == Some(UpdateOperation::PreparingInstall)
            || runtime.operation == Some(UpdateOperation::Installing)
        {
            runtime.operation = None;
        }
        runtime.state.phase = UpdatePhase::Ready;
        runtime.state.error = Some(message.into());
        runtime.state.clone()
    }

    fn postpone(&self) -> AppResult<UpdateState> {
        let mut runtime = lock_unpoisoned(&self.runtime);
        ensure_idle_operation(&runtime)?;
        runtime.postponed_version = runtime.state.available_version.clone();
        runtime.state.error = None;
        #[cfg(windows)]
        if runtime.package.is_some() {
            runtime.state.phase = UpdatePhase::Ready;
            return Ok(runtime.state.clone());
        }
        runtime.state.phase = if runtime.state.available_version.is_some() {
            UpdatePhase::Available
        } else {
            UpdatePhase::Idle
        };
        Ok(runtime.state.clone())
    }

    fn automatic_action(&self) -> AutomaticAction {
        let runtime = lock_unpoisoned(&self.runtime);
        if runtime.operation.is_some() {
            return AutomaticAction::None;
        }
        #[cfg(windows)]
        {
            if runtime.package.is_some() {
                return AutomaticAction::None;
            }
            if runtime.update.is_some() {
                if runtime.postponed_version == runtime.state.available_version {
                    return AutomaticAction::None;
                }
                return AutomaticAction::Download;
            }
        }
        AutomaticAction::Check
    }

    fn begin_preparation(&self, pending_windows: HashSet<String>) -> AppResult<String> {
        let mut preparation = lock_unpoisoned(&self.preparation);
        if preparation.is_some() {
            return Err(AppError::new(
                "update_prepare_busy",
                "另一个更新安装准备任务正在进行",
            ));
        }
        let request_id = Uuid::new_v4().to_string();
        *preparation = Some(InstallPreparation {
            request_id: request_id.clone(),
            pending_windows,
            failure: None,
        });
        Ok(request_id)
    }

    fn register_window(&self, window_label: &str) {
        if window_requires_install_preparation(window_label) {
            lock_unpoisoned(&self.registered_windows).insert(window_label.to_string());
        }
    }

    fn unregister_window(&self, window_label: &str) {
        lock_unpoisoned(&self.registered_windows).remove(window_label);
        let mut preparation = lock_unpoisoned(&self.preparation);
        if let Some(active) = preparation.as_mut() {
            if active.pending_windows.remove(window_label) && active.failure.is_none() {
                active.failure = Some(format!("{window_label}: 窗口在完成更新前保存之前已经关闭"));
            }
        }
        drop(preparation);
        self.preparation_notify.notify_one();
    }

    fn registered_windows(&self) -> HashSet<String> {
        lock_unpoisoned(&self.registered_windows).clone()
    }

    fn window_is_registered(&self, window_label: &str) -> bool {
        lock_unpoisoned(&self.registered_windows).contains(window_label)
    }

    fn acknowledge_preparation(
        &self,
        request_id: &str,
        window_label: &str,
        ok: bool,
        error: Option<String>,
    ) -> AppResult<()> {
        let mut preparation = lock_unpoisoned(&self.preparation);
        let active = preparation
            .as_mut()
            .ok_or_else(|| AppError::not_found("更新安装准备任务不存在或已经结束"))?;
        if active.request_id != request_id {
            return Err(AppError::new(
                "update_prepare_replaced",
                "更新安装准备任务已被替换",
            ));
        }
        if !active.pending_windows.remove(window_label) {
            return Err(AppError::invalid("当前窗口不在更新安装准备列表中"));
        }
        if !ok && active.failure.is_none() {
            let detail = error
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "保存或清理失败".to_string());
            active.failure = Some(format!("{window_label}: {detail}"));
        }
        drop(preparation);
        // There is only one installation waiter. `notify_one` retains a permit
        // when an acknowledgement lands between the status check and `notified()`.
        self.preparation_notify.notify_one();
        Ok(())
    }

    fn preparation_status(&self, request_id: &str) -> PreparationStatus {
        let preparation = lock_unpoisoned(&self.preparation);
        let Some(active) = preparation.as_ref() else {
            return PreparationStatus::Replaced;
        };
        if active.request_id != request_id {
            return PreparationStatus::Replaced;
        }
        if let Some(failure) = &active.failure {
            return PreparationStatus::Failed(failure.clone());
        }
        if active.pending_windows.is_empty() {
            return PreparationStatus::Complete;
        }
        let mut pending = active.pending_windows.iter().cloned().collect::<Vec<_>>();
        pending.sort();
        PreparationStatus::Pending(pending)
    }

    fn remove_preparation_window(&self, request_id: &str, window_label: &str) {
        let mut preparation = lock_unpoisoned(&self.preparation);
        if let Some(active) = preparation
            .as_mut()
            .filter(|active| active.request_id == request_id)
        {
            active.pending_windows.remove(window_label);
        }
        drop(preparation);
        self.preparation_notify.notify_one();
    }

    fn clear_preparation(&self, request_id: &str) {
        let mut preparation = lock_unpoisoned(&self.preparation);
        if preparation
            .as_ref()
            .is_some_and(|active| active.request_id == request_id)
        {
            *preparation = None;
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn ensure_idle_operation(runtime: &UpdateRuntime) -> AppResult<()> {
    if runtime.operation.is_some() {
        return Err(AppError::new("update_busy", "更新操作正在进行，请稍候再试"));
    }
    return Ok(());
}

fn emit_state(app: &AppHandle, state: &UpdateState) {
    let _ = app.emit(UPDATE_STATE_EVENT, state.clone());
}

#[tauri::command]
pub fn get_update_settings(store: State<'_, WorkspaceStore>) -> UpdateSettings {
    UpdateSettings {
        auto_update: store.auto_update_enabled(),
    }
}

#[tauri::command]
pub async fn set_update_settings(
    app: AppHandle,
    settings: UpdateSettings,
) -> AppResult<UpdateSettings> {
    let auto_update = settings.auto_update;
    let app_for_storage = app.clone();
    crate::commands::run_background("保存自动更新设置", move || {
        app_for_storage
            .state::<WorkspaceStore>()
            .set_auto_update_enabled(auto_update)
    })
    .await?;

    #[cfg(windows)]
    if auto_update {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            // One-shot kick when the user enables auto-update; the scheduler
            // owns retries, and failures are already logged inside.
            let _ = run_automatic_cycle(app).await;
        });
    }

    Ok(settings)
}

#[tauri::command]
pub fn get_update_state(manager: State<'_, UpdaterManager>) -> UpdateState {
    manager.state()
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> AppResult<UpdateState> {
    #[cfg(windows)]
    {
        perform_check(app, false).await
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err(updater_unsupported())
    }
}

#[tauri::command]
pub async fn download_update(app: AppHandle) -> AppResult<UpdateState> {
    #[cfg(windows)]
    {
        perform_download(app).await
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err(updater_unsupported())
    }
}

#[tauri::command]
pub fn postpone_update(app: AppHandle) -> AppResult<UpdateState> {
    #[cfg(windows)]
    {
        let state = app.state::<UpdaterManager>().postpone()?;
        emit_state(&app, &state);
        Ok(state)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err(updater_unsupported())
    }
}

#[tauri::command]
pub fn register_update_install_window(window: WebviewWindow, app: AppHandle) {
    app.state::<UpdaterManager>()
        .register_window(window.label());
}

#[tauri::command]
pub fn unregister_update_install_window(window: WebviewWindow, app: AppHandle) {
    app.state::<UpdaterManager>()
        .unregister_window(window.label());
}

#[tauri::command]
pub fn acknowledge_update_install(
    window: WebviewWindow,
    app: AppHandle,
    request_id: String,
    window_label: String,
    ok: bool,
    error: Option<String>,
) -> AppResult<()> {
    if window.label() != window_label {
        return Err(AppError::invalid("更新安装确认的窗口标识与调用窗口不一致"));
    }
    app.state::<UpdaterManager>()
        .acknowledge_preparation(&request_id, &window_label, ok, error)
}

#[tauri::command]
pub async fn install_update_and_restart(window: WebviewWindow, app: AppHandle) -> AppResult<()> {
    #[cfg(windows)]
    {
        if !app
            .state::<UpdaterManager>()
            .window_is_registered(window.label())
        {
            return Err(AppError::new(
                "update_window_not_ready",
                "当前窗口尚未完成更新前保存准备，请稍后重试",
            ));
        }
        install_update_windows(app).await
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        let _ = app;
        Err(updater_unsupported())
    }
}

pub fn start_scheduler(app: AppHandle) {
    #[cfg(windows)]
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_CHECK_DELAY).await;
        let mut retry_delay = FAILURE_RETRY_BASE_DELAY;
        loop {
            let delay = if run_automatic_cycle(app.clone()).await.is_err() {
                let delay = retry_delay;
                retry_delay = (retry_delay * 2).min(FAILURE_RETRY_MAX_DELAY);
                delay
            } else {
                retry_delay = FAILURE_RETRY_BASE_DELAY;
                PERIODIC_CHECK_INTERVAL
            };
            tokio::time::sleep(delay).await;
        }
    });

    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(not(windows))]
fn updater_unsupported() -> AppError {
    AppError::new(
        "updater_unsupported",
        "当前版本仅支持 Windows 客户端自动更新",
    )
}

#[cfg(windows)]
async fn perform_check(app: AppHandle, download_if_available: bool) -> AppResult<UpdateState> {
    let manager = app.state::<UpdaterManager>();
    let checking = manager.begin_check()?;
    emit_state(&app, &checking);

    let result = match app.updater() {
        Ok(updater) => updater.check().await.map_err(|error| {
            AppError::new("update_check_failed", format!("检查更新失败: {error}"))
        }),
        Err(error) => Err(AppError::new(
            "update_configuration_error",
            format!("初始化更新服务失败: {error}"),
        )),
    };

    let state = match result {
        Ok(Some(update)) => manager.finish_available(update),
        Ok(None) => manager.finish_up_to_date(),
        Err(error) => {
            let state = manager.finish_error(error.message.clone());
            emit_state(&app, &state);
            return Err(error);
        }
    };
    emit_state(&app, &state);

    if download_if_available && state.phase == UpdatePhase::Available {
        return perform_download(app).await;
    }
    Ok(state)
}

#[cfg(windows)]
async fn perform_download(app: AppHandle) -> AppResult<UpdateState> {
    let manager = app.state::<UpdaterManager>();
    let (update, downloading) = manager.begin_download()?;
    let Some(update) = update else {
        emit_state(&app, &downloading);
        return Ok(downloading);
    };
    emit_state(&app, &downloading);

    let progress_app = app.clone();
    let download_result = update
        .download(
            move |chunk_bytes, total_bytes| {
                if let Some(state) = progress_app
                    .state::<UpdaterManager>()
                    .record_download_progress(chunk_bytes, total_bytes)
                {
                    emit_state(&progress_app, &state);
                }
            },
            || {},
        )
        .await;

    match download_result {
        Ok(bytes) => {
            let state = manager.finish_download(bytes);
            emit_state(&app, &state);
            Ok(state)
        }
        Err(error) => {
            let message = format!("下载或验证更新包失败: {error}");
            let state = manager.finish_error(message.clone());
            emit_state(&app, &state);
            Err(AppError::new("update_download_failed", message))
        }
    }
}

#[cfg(windows)]
/// Runs one automatic check/download cycle.
///
/// Reports failure to the caller so the scheduler can back off; a disabled
/// updater or an idle action counts as success, not as something to retry.
async fn run_automatic_cycle(app: AppHandle) -> Result<(), ()> {
    if !app.state::<WorkspaceStore>().auto_update_enabled() {
        return Ok(());
    }
    let action = app.state::<UpdaterManager>().automatic_action();
    let result = match action {
        AutomaticAction::None => return Ok(()),
        AutomaticAction::Check => perform_check(app.clone(), true).await.map(|_| ()),
        AutomaticAction::Download => perform_download(app.clone()).await.map(|_| ()),
    };
    result.map_err(|error| {
        eprintln!("自动更新任务失败: {error}");
    })
}

#[cfg(windows)]
async fn install_update_windows(app: AppHandle) -> AppResult<()> {
    let manager = app.state::<UpdaterManager>();
    manager.begin_install_preparation()?;

    let registered_windows = manager.registered_windows();
    let pending_windows = app
        .webview_windows()
        .keys()
        .filter(|label| {
            window_requires_install_preparation(label) && registered_windows.contains(*label)
        })
        .cloned()
        .collect::<HashSet<_>>();
    let request_id = match manager.begin_preparation(pending_windows.clone()) {
        Ok(request_id) => request_id,
        Err(error) => {
            manager.abort_install_preparation(error.message.clone());
            return Err(error);
        }
    };
    let payload = InstallPreparationEvent {
        request_id: request_id.clone(),
    };
    for label in pending_windows {
        if app.get_webview_window(&label).is_none() {
            manager.remove_preparation_window(&request_id, &label);
            continue;
        }
        if let Err(error) = app.emit_to(&label, PREPARE_INSTALL_EVENT, payload.clone()) {
            manager.clear_preparation(&request_id);
            let message = format!("通知窗口 {label} 保存数据失败: {error}");
            let state = manager.abort_install_preparation(message.clone());
            emit_state(&app, &state);
            return Err(AppError::new("update_prepare_failed", message));
        }
    }

    if let Err(error) = wait_for_install_preparation(&manager, &request_id).await {
        manager.clear_preparation(&request_id);
        let state = manager.abort_install_preparation(error.message.clone());
        emit_state(&app, &state);
        return Err(error);
    }
    manager.clear_preparation(&request_id);

    let (update, package, installing) = match manager.mark_installing() {
        Ok(value) => value,
        Err(error) => {
            let state = manager.abort_install_preparation(error.message.clone());
            emit_state(&app, &state);
            return Err(error);
        }
    };
    emit_state(&app, &installing);

    let install_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        install_app.state::<crate::mfa::MfaStore>().lock();
        crate::long_screenshot::shutdown(&install_app);
        crate::screenshot::shutdown(&install_app);
        terminate_native_host_for_update()?;
        update.install(package.as_slice()).map_err(|error| {
            AppError::new(
                "update_install_failed",
                format!("启动更新安装失败: {error}"),
            )
        })
    })
    .await
    .map_err(|error| {
        AppError::new(
            "update_install_task_failed",
            format!("更新安装任务异常结束: {error}"),
        )
    })?;

    if let Err(error) = result {
        let state = manager.abort_install_preparation(error.message.clone());
        emit_state(&app, &state);
        return Err(error);
    }
    Ok(())
}

#[cfg(windows)]
async fn wait_for_install_preparation(manager: &UpdaterManager, request_id: &str) -> AppResult<()> {
    let deadline = tokio::time::Instant::now() + INSTALL_PREPARATION_TIMEOUT;
    loop {
        match manager.preparation_status(request_id) {
            PreparationStatus::Complete => return Ok(()),
            PreparationStatus::Failed(error) => {
                return Err(AppError::new(
                    "update_prepare_failed",
                    format!("更新前保存或清理失败: {error}"),
                ));
            }
            PreparationStatus::Replaced => {
                return Err(AppError::new(
                    "update_prepare_replaced",
                    "更新安装准备任务不存在或已被替换",
                ));
            }
            PreparationStatus::Pending(pending) => {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(AppError::new(
                        "update_prepare_timeout",
                        format!("等待窗口保存数据超时: {}", pending.join(", ")),
                    ));
                }
                let notified = manager.preparation_notify.notified();
                if tokio::time::timeout(deadline - now, notified)
                    .await
                    .is_err()
                {
                    return Err(AppError::new(
                        "update_prepare_timeout",
                        format!("等待窗口保存数据超时: {}", pending.join(", ")),
                    ));
                }
            }
        }
    }
}

fn window_requires_install_preparation(label: &str) -> bool {
    label == "main"
        || label.starts_with("note-")
        || matches!(
            label,
            "timer" | "reminder" | "gantt" | "mfa" | "screenshot-capture"
        )
        || label.starts_with("screenshot-long-")
}

#[cfg(windows)]
fn terminate_native_host_for_update() -> AppResult<()> {
    use std::mem::size_of;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    struct Handle(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    let current_exe =
        std::env::current_exe().map_err(|error| AppError::io("定位飞花程序目录", error))?;
    let expected = current_exe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("petaldesk-browser-host.exe");
    if !expected.exists() {
        return Ok(());
    }

    let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) });
    if snapshot.0 == INVALID_HANDLE_VALUE {
        return Err(AppError::io(
            "枚举浏览器集成进程",
            std::io::Error::last_os_error(),
        ));
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let mut has_entry = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while has_entry {
        let exe_name = wide_nul_to_string(&entry.szExeFile);
        if exe_name.eq_ignore_ascii_case("petaldesk-browser-host.exe") {
            let access =
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE;
            let process = Handle(unsafe { OpenProcess(access, 0, entry.th32ProcessID) });
            if !process.0.is_null() {
                if let Some(candidate) = query_process_path(process.0) {
                    if native_host_paths_match(&candidate, &expected) {
                        if unsafe { TerminateProcess(process.0, 0) } == 0 {
                            return Err(AppError::io(
                                "结束浏览器集成进程",
                                std::io::Error::last_os_error(),
                            ));
                        }
                        unsafe {
                            WaitForSingleObject(process.0, 2_000);
                        }
                    }
                }
            }
        }
        has_entry = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    return Ok(());

    fn query_process_path(process: windows_sys::Win32::Foundation::HANDLE) -> Option<PathBuf> {
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0
        {
            return None;
        }
        buffer.truncate(length as usize);
        Some(PathBuf::from(String::from_utf16_lossy(&buffer)))
    }
}

#[cfg(windows)]
fn wide_nul_to_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

#[cfg(windows)]
fn native_host_paths_match(candidate: &std::path::Path, expected: &std::path::Path) -> bool {
    fn display_path(path: &std::path::Path) -> String {
        let value = path.to_string_lossy().replace('/', "\\");
        value
            .strip_prefix(r"\\?\")
            .unwrap_or(&value)
            .trim_end_matches('\\')
            .to_string()
    }
    display_path(candidate).eq_ignore_ascii_case(&display_path(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_automatic_updates() {
        assert!(UpdateSettings::default().auto_update);
        let value = serde_json::to_value(UpdateSettings::default()).unwrap();
        assert_eq!(value, serde_json::json!({ "autoUpdate": true }));
    }

    #[test]
    fn update_phase_uses_frontend_camel_case_values() {
        assert_eq!(
            serde_json::to_value(UpdatePhase::UpToDate).unwrap(),
            serde_json::json!("upToDate")
        );
    }

    #[test]
    fn concurrent_operations_are_rejected_without_holding_a_lock_across_work() {
        let manager = UpdaterManager::default();
        assert_eq!(manager.begin_check().unwrap().phase, UpdatePhase::Checking);
        let error = manager.begin_check().unwrap_err();
        assert_eq!(error.code, "update_busy");
        assert_eq!(manager.finish_up_to_date().phase, UpdatePhase::UpToDate);
    }

    #[test]
    fn progress_is_monotonic_and_preserves_the_reported_total() {
        let manager = UpdaterManager::default();
        {
            let mut runtime = lock_unpoisoned(&manager.runtime);
            runtime.operation = Some(UpdateOperation::Downloading);
            runtime.state.phase = UpdatePhase::Downloading;
            runtime.last_progress_emit = None;
        }
        let first = manager.record_download_progress(512, Some(1024)).unwrap();
        assert_eq!(first.downloaded_bytes, 512);
        assert_eq!(first.total_bytes, Some(1024));
        let second = manager.record_download_progress(512, None).unwrap();
        assert_eq!(second.downloaded_bytes, 1024);
        assert_eq!(second.total_bytes, Some(1024));
    }

    #[cfg(windows)]
    #[test]
    fn postponing_a_downloaded_update_keeps_it_ready_for_later_installation() {
        let manager = UpdaterManager::default();
        {
            let mut runtime = lock_unpoisoned(&manager.runtime);
            runtime.state.phase = UpdatePhase::Ready;
            runtime.state.available_version = Some("9.9.9".to_string());
            runtime.package = Some(Arc::new(vec![1, 2, 3]));
        }

        let state = manager.postpone().unwrap();

        assert_eq!(state.phase, UpdatePhase::Ready);
        assert_eq!(state.available_version.as_deref(), Some("9.9.9"));
        assert!(matches!(manager.automatic_action(), AutomaticAction::None));
    }

    #[test]
    fn install_preparation_requires_every_expected_window() {
        let manager = UpdaterManager::default();
        let request = manager
            .begin_preparation(HashSet::from(["main".to_string(), "note-a".to_string()]))
            .unwrap();
        manager
            .acknowledge_preparation(&request, "main", true, None)
            .unwrap();
        match manager.preparation_status(&request) {
            PreparationStatus::Pending(labels) => assert_eq!(labels, vec!["note-a"]),
            _ => panic!("expected one pending window"),
        }
        manager
            .acknowledge_preparation(&request, "note-a", true, None)
            .unwrap();
        assert!(matches!(
            manager.preparation_status(&request),
            PreparationStatus::Complete
        ));
    }

    #[test]
    fn install_preparation_propagates_window_failures() {
        let manager = UpdaterManager::default();
        let request = manager
            .begin_preparation(HashSet::from(["gantt".to_string()]))
            .unwrap();
        manager
            .acknowledge_preparation(&request, "gantt", false, Some("任务保存失败".to_string()))
            .unwrap();
        match manager.preparation_status(&request) {
            PreparationStatus::Failed(message) => {
                assert!(message.contains("gantt"));
                assert!(message.contains("任务保存失败"));
            }
            _ => panic!("expected a preparation failure"),
        }
    }

    #[test]
    fn only_stateful_windows_block_update_installation() {
        for label in [
            "main",
            "note-123",
            "timer",
            "reminder",
            "gantt",
            "mfa",
            "screenshot-capture",
            "screenshot-long-control",
            "screenshot-long-outline",
        ] {
            assert!(window_requires_install_preparation(label), "{label}");
        }
        assert!(!window_requires_install_preparation("screenshot-pin-123"));
        assert!(!window_requires_install_preparation("unrelated"));
    }

    #[test]
    fn installation_waits_only_for_windows_with_ready_listeners() {
        let manager = UpdaterManager::default();
        manager.register_window("main");
        manager.register_window("gantt");
        manager.register_window("screenshot-pin-123");
        manager.unregister_window("gantt");

        assert_eq!(
            manager.registered_windows(),
            HashSet::from(["main".to_string()])
        );
        assert!(manager.window_is_registered("main"));
        assert!(!manager.window_is_registered("gantt"));
    }

    #[test]
    fn closing_a_pending_window_fails_preparation_without_waiting_for_timeout() {
        let manager = UpdaterManager::default();
        manager.register_window("gantt");
        let request = manager
            .begin_preparation(HashSet::from(["gantt".to_string()]))
            .unwrap();

        manager.unregister_window("gantt");

        match manager.preparation_status(&request) {
            PreparationStatus::Failed(message) => {
                assert!(message.contains("gantt"));
                assert!(message.contains("已经关闭"));
            }
            _ => panic!("expected closing a pending window to fail preparation"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_host_path_match_is_case_insensitive_and_accepts_extended_prefix() {
        let expected =
            std::path::Path::new(r"C:\Program Files\PetalDesk\petaldesk-browser-host.exe");
        assert!(native_host_paths_match(
            std::path::Path::new(r"\\?\c:\program files\PETALDESK\petaldesk-browser-host.exe"),
            expected
        ));
        assert!(!native_host_paths_match(
            std::path::Path::new(r"C:\Temp\petaldesk-browser-host.exe"),
            expected
        ));
    }
}
