use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;

pub const AUTOSTART_ARGUMENT: &str = "--autostart";
const AUTOSTART_VALUE_NAME: &str = "PetalDesk";
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED_KEY_PATH: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
static LAUNCH_AT_LOGIN_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchAtLoginStatus {
    pub supported: bool,
    pub enabled: bool,
}

pub fn args_contain_autostart<I, S>(arguments: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    arguments
        .into_iter()
        .any(|argument| argument.as_ref() == AUTOSTART_ARGUMENT)
}

pub fn current_launch_is_autostart() -> bool {
    args_contain_autostart(std::env::args())
}

fn launch_command(executable: &Path) -> String {
    // Windows paths cannot contain a literal double quote. Always quoting the
    // executable keeps the Run value correct for user profiles and install
    // directories that contain spaces.
    format!("\"{}\" {AUTOSTART_ARGUMENT}", executable.to_string_lossy())
}

trait LaunchAtLoginRegistry {
    fn read_run_value(&self) -> std::io::Result<Option<String>>;
    fn write_run_value(&self, value: &str) -> std::io::Result<()>;
    fn delete_run_value(&self) -> std::io::Result<()>;
    fn read_startup_approved(&self) -> std::io::Result<Option<Vec<u8>>>;
    fn delete_startup_approved(&self) -> std::io::Result<()>;
}

fn startup_approved_is_disabled(value: Option<&[u8]>) -> bool {
    // Explorer writes 2 for enabled and 3 for disabled in the first DWORD.
    // A missing value means the corresponding Run entry is enabled.
    value.and_then(|bytes| bytes.first()).copied() == Some(3)
}

fn enabled_with_registry(
    registry: &impl LaunchAtLoginRegistry,
    expected_command: &str,
) -> std::io::Result<bool> {
    let Some(run_value) = registry.read_run_value()? else {
        return Ok(false);
    };
    if run_value != expected_command {
        return Ok(false);
    }
    Ok(!startup_approved_is_disabled(
        registry.read_startup_approved()?.as_deref(),
    ))
}

fn set_enabled_with_registry(
    registry: &impl LaunchAtLoginRegistry,
    expected_command: &str,
    enabled: bool,
) -> std::io::Result<bool> {
    if enabled {
        registry.write_run_value(expected_command)?;
        if let Err(error) = registry.delete_startup_approved() {
            // Keep a failed enable operation fail-safe: do not leave behind a
            // Run entry whose Task Manager approval state could not be reset.
            let _ = registry.delete_run_value();
            return Err(error);
        }
    } else {
        registry.delete_run_value()?;
        // With the Run value gone the app cannot start. Cleanup of Explorer's
        // historical approval marker is useful but should not turn a completed
        // disable operation into a misleading failure.
        let _ = registry.delete_startup_approved();
    }

    match enabled_with_registry(registry, expected_command) {
        Ok(actual) if actual == enabled => Ok(actual),
        Ok(_) => {
            if enabled {
                let _ = registry.delete_run_value();
            }
            Err(std::io::Error::other("Windows 未能应用开机启动设置"))
        }
        Err(error) => {
            if enabled {
                let _ = registry.delete_run_value();
            }
            Err(error)
        }
    }
}

fn launch_at_login_error(action: &str, error: std::io::Error) -> AppError {
    AppError::new("launch_at_login_error", format!("{action}失败: {error}"))
}

#[cfg(windows)]
mod windows_registry {
    use super::*;
    use std::io;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::{RegKey, RegValue};

    pub(super) struct WindowsLaunchAtLoginRegistry;

    fn optional_subkey(path: &str) -> io::Result<Option<RegKey>> {
        match RegKey::predef(HKEY_CURRENT_USER).open_subkey(path) {
            Ok(key) => Ok(Some(key)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn delete_optional_value(key: &RegKey, name: &str) -> io::Result<()> {
        match key.delete_value(name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    impl LaunchAtLoginRegistry for WindowsLaunchAtLoginRegistry {
        fn read_run_value(&self) -> io::Result<Option<String>> {
            let Some(key) = optional_subkey(RUN_KEY_PATH)? else {
                return Ok(None);
            };
            match key.get_value(AUTOSTART_VALUE_NAME) {
                Ok(value) => Ok(Some(value)),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }

        fn write_run_value(&self, value: &str) -> io::Result<()> {
            let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY_PATH)?;
            key.set_value(AUTOSTART_VALUE_NAME, &value)
        }

        fn delete_run_value(&self) -> io::Result<()> {
            let Some(key) = optional_subkey(RUN_KEY_PATH)? else {
                return Ok(());
            };
            delete_optional_value(&key, AUTOSTART_VALUE_NAME)
        }

        fn read_startup_approved(&self) -> io::Result<Option<Vec<u8>>> {
            let Some(key) = optional_subkey(STARTUP_APPROVED_KEY_PATH)? else {
                return Ok(None);
            };
            match key.get_raw_value(AUTOSTART_VALUE_NAME) {
                Ok(RegValue { bytes, .. }) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }

        fn delete_startup_approved(&self) -> io::Result<()> {
            let Some(key) = optional_subkey(STARTUP_APPROVED_KEY_PATH)? else {
                return Ok(());
            };
            delete_optional_value(&key, AUTOSTART_VALUE_NAME)
        }
    }
}

pub fn get_status() -> AppResult<LaunchAtLoginStatus> {
    #[cfg(windows)]
    {
        let _guard = LAUNCH_AT_LOGIN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let executable = std::env::current_exe()
            .map_err(|error| launch_at_login_error("读取飞花程序路径", error))?;
        let command = launch_command(&executable);
        let enabled =
            enabled_with_registry(&windows_registry::WindowsLaunchAtLoginRegistry, &command)
                .map_err(|error| launch_at_login_error("读取开机启动设置", error))?;
        return Ok(LaunchAtLoginStatus {
            supported: true,
            enabled,
        });
    }

    #[cfg(not(windows))]
    Ok(LaunchAtLoginStatus {
        supported: false,
        enabled: false,
    })
}

pub fn set_enabled(enabled: bool) -> AppResult<LaunchAtLoginStatus> {
    #[cfg(windows)]
    {
        let _guard = LAUNCH_AT_LOGIN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let executable = std::env::current_exe()
            .map_err(|error| launch_at_login_error("读取飞花程序路径", error))?;
        let command = launch_command(&executable);
        let enabled = set_enabled_with_registry(
            &windows_registry::WindowsLaunchAtLoginRegistry,
            &command,
            enabled,
        )
        .map_err(|error| launch_at_login_error("设置开机启动", error))?;
        return Ok(LaunchAtLoginStatus {
            supported: true,
            enabled,
        });
    }

    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err(AppError::new(
            "unsupported_platform",
            "开机启动仅支持 Windows 桌面版。",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeRegistry {
        run_value: RefCell<Option<String>>,
        startup_approved: RefCell<Option<Vec<u8>>>,
    }

    impl FakeRegistry {
        fn with_values(run_value: Option<&str>, startup_approved: Option<Vec<u8>>) -> Self {
            Self {
                run_value: RefCell::new(run_value.map(ToOwned::to_owned)),
                startup_approved: RefCell::new(startup_approved),
            }
        }
    }

    impl LaunchAtLoginRegistry for FakeRegistry {
        fn read_run_value(&self) -> std::io::Result<Option<String>> {
            Ok(self.run_value.borrow().clone())
        }

        fn write_run_value(&self, value: &str) -> std::io::Result<()> {
            *self.run_value.borrow_mut() = Some(value.to_owned());
            Ok(())
        }

        fn delete_run_value(&self) -> std::io::Result<()> {
            *self.run_value.borrow_mut() = None;
            Ok(())
        }

        fn read_startup_approved(&self) -> std::io::Result<Option<Vec<u8>>> {
            Ok(self.startup_approved.borrow().clone())
        }

        fn delete_startup_approved(&self) -> std::io::Result<()> {
            *self.startup_approved.borrow_mut() = None;
            Ok(())
        }
    }

    #[test]
    fn launch_command_quotes_executable_paths_with_spaces() {
        assert_eq!(
            launch_command(Path::new(r"C:\Program Files\PetalDesk\PetalDesk.exe")),
            r#""C:\Program Files\PetalDesk\PetalDesk.exe" --autostart"#
        );
    }

    #[test]
    fn status_requires_the_current_exact_run_value() {
        let expected = r#""C:\Apps\PetalDesk.exe" --autostart"#;
        assert!(
            enabled_with_registry(&FakeRegistry::with_values(Some(expected), None), expected)
                .unwrap()
        );
        assert!(!enabled_with_registry(
            &FakeRegistry::with_values(Some(r#""D:\Old\PetalDesk.exe" --autostart"#), None),
            expected
        )
        .unwrap());
        assert!(!enabled_with_registry(&FakeRegistry::default(), expected).unwrap());
    }

    #[test]
    fn task_manager_disabled_state_is_reported_as_off() {
        let expected = r#""C:\Apps\PetalDesk.exe" --autostart"#;
        let registry = FakeRegistry::with_values(Some(expected), Some(vec![3, 0, 0, 0]));
        assert!(!enabled_with_registry(&registry, expected).unwrap());
    }

    #[test]
    fn enabling_writes_the_exact_command_and_clears_old_approval_state() {
        let expected = r#""C:\Apps\PetalDesk.exe" --autostart"#;
        let registry = FakeRegistry::with_values(None, Some(vec![3, 0, 0, 0]));
        assert!(set_enabled_with_registry(&registry, expected, true).unwrap());
        assert_eq!(registry.run_value.borrow().as_deref(), Some(expected));
        assert!(registry.startup_approved.borrow().is_none());
    }

    #[test]
    fn disabling_removes_the_run_and_approval_values() {
        let expected = r#""C:\Apps\PetalDesk.exe" --autostart"#;
        let registry = FakeRegistry::with_values(Some(expected), Some(vec![2, 0, 0, 0]));
        assert!(!set_enabled_with_registry(&registry, expected, false).unwrap());
        assert!(registry.run_value.borrow().is_none());
        assert!(registry.startup_approved.borrow().is_none());
    }

    #[test]
    fn autostart_argument_detection_is_exact() {
        assert!(args_contain_autostart(["PetalDesk.exe", "--autostart"]));
        assert!(!args_contain_autostart([
            "PetalDesk.exe",
            "--autostart=true"
        ]));
        assert!(!args_contain_autostart(["PetalDesk.exe"]));
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_platform_reports_a_disabled_status() {
        assert_eq!(
            get_status().unwrap(),
            LaunchAtLoginStatus {
                supported: false,
                enabled: false,
            }
        );
    }
}
