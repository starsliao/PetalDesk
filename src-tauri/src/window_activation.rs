use crate::error::{AppError, AppResult};
use windows_sys::Win32::System::Threading::AttachThreadInput;

/// Temporarily joins two Windows input queues so foreground activation can be
/// retried without leaving the threads attached after an early return.
pub(crate) struct ThreadInputAttachment {
    source: u32,
    target: u32,
}

impl ThreadInputAttachment {
    pub(crate) fn attach(
        source: u32,
        target: u32,
        action: &'static str,
    ) -> AppResult<Option<Self>> {
        if source == 0 || target == 0 || source == target {
            return Ok(None);
        }
        if unsafe { AttachThreadInput(source, target, 1) } == 0 {
            return Err(AppError::new(
                "windows_error",
                format!("{action}失败: {}", std::io::Error::last_os_error()),
            ));
        }
        Ok(Some(Self { source, target }))
    }
}

impl Drop for ThreadInputAttachment {
    fn drop(&mut self) {
        unsafe {
            let _ = AttachThreadInput(self.source, self.target, 0);
        }
    }
}
