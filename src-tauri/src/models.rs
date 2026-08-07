use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const SCHEMA_VERSION: u32 = 3;
pub const ALLOWED_COLORS: &[&str] = &[
    "yellow", "pink", "blue", "green", "purple", "gray", "charcoal",
];
pub const DEFAULT_EDITOR_MODE: &str = "typora";
pub const ALLOWED_EDITOR_MODES: &[&str] = &["typora", "plain"];
pub const DEFAULT_NOTE_TITLE: &str = "无标题便签";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolName {
    Timer,
    Reminder,
    Gantt,
    Mfa,
    Passwords,
    Screenshot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TrayShortcutAction {
    FirstNote,
    RecentNote,
    MainWindow,
    Timer,
    Reminder,
    Gantt,
    Mfa,
    Passwords,
    Screenshot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrayShortcutSettings {
    #[serde(default = "default_tray_double_click")]
    pub double_click: TrayShortcutAction,
    #[serde(default = "default_tray_alt_double_click")]
    pub alt_double_click: TrayShortcutAction,
    #[serde(default = "default_tray_ctrl_double_click")]
    pub ctrl_double_click: TrayShortcutAction,
    #[serde(default = "default_tray_shift_double_click")]
    pub shift_double_click: TrayShortcutAction,
}

impl Default for TrayShortcutSettings {
    fn default() -> Self {
        Self {
            double_click: default_tray_double_click(),
            alt_double_click: default_tray_alt_double_click(),
            ctrl_double_click: default_tray_ctrl_double_click(),
            shift_double_click: default_tray_shift_double_click(),
        }
    }
}

fn default_tray_double_click() -> TrayShortcutAction {
    TrayShortcutAction::FirstNote
}

fn default_tray_alt_double_click() -> TrayShortcutAction {
    TrayShortcutAction::Gantt
}

fn default_tray_ctrl_double_click() -> TrayShortcutAction {
    TrayShortcutAction::Mfa
}

fn default_tray_shift_double_click() -> TrayShortcutAction {
    TrayShortcutAction::MainWindow
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NoteMeta {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_editor_mode")]
    pub editor_mode: String,
    pub color: String,
    pub pinned: bool,
    #[serde(default)]
    pub read_only: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub content_hash: String,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NoteSnapshot {
    pub id: String,
    pub revision: u64,
    pub content_hash: String,
    pub markdown: String,
    pub meta: NoteMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub excerpt: String,
    pub editor_mode: String,
    pub color: String,
    pub pinned: bool,
    pub read_only: bool,
    pub created_at: String,
    pub updated_at: String,
    pub schema_version: u32,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NoteMetaPatch {
    pub title: Option<String>,
    pub editor_mode: Option<String>,
    pub color: Option<String>,
    pub pinned: Option<bool>,
    pub read_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommitNoteRequest {
    pub id: String,
    pub base_revision: u64,
    #[serde(default)]
    pub base_content_hash: Option<String>,
    pub markdown: String,
    #[serde(default)]
    pub meta_patch: NoteMetaPatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub revision: u64,
    pub saved_at: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssetResult {
    pub relative_path: String,
    pub asset_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssetContent {
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(flatten)]
    pub note: NoteSummary,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveredDraft {
    pub note_id: String,
    pub status: String,
    pub recovered_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub path: String,
    pub recovered_drafts: Vec<RecoveredDraft>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DataStorageChangeResult {
    pub path: String,
    pub restart_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub build_timestamp: u64,
    pub workspace_path: String,
    pub default_editor_mode: String,
    pub tray_shortcut_settings: TrayShortcutSettings,
    pub protect_sensitive_windows: bool,
    pub colors: Vec<String>,
    pub recovered_drafts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredWindowStates {
    #[serde(default)]
    pub windows: HashMap<String, WindowState>,
    #[serde(default)]
    pub open_notes: Vec<String>,
    #[serde(default)]
    pub last_note_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub workspace_path: String,
    #[serde(default = "default_editor_mode", alias = "editorMode")]
    pub default_editor_mode: String,
}

pub fn default_editor_mode() -> String {
    DEFAULT_EDITOR_MODE.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub note_id: String,
    pub base_revision: u64,
    #[serde(default)]
    pub base_content_hash: Option<String>,
    pub new_revision: u64,
    pub markdown: String,
    pub meta: NoteMeta,
    pub created_at: String,
}
