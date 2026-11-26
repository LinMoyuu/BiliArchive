use std::io;

pub(crate) struct DesktopLock {
    #[cfg(windows)]
    handle: std::ptr::NonNull<std::ffi::c_void>,
    #[cfg(not(windows))]
    _file: std::fs::File,
}

pub(crate) fn acquire_or_notify() -> Option<DesktopLock> {
    let (level, message) = match DesktopLock::try_acquire() {
        Ok(Some(lock)) => return Some(lock),
        Ok(None) => (rfd::MessageLevel::Info, "BiliArchive 已在运行。".to_owned()),
        Err(error) => (
            rfd::MessageLevel::Error,
            format!("无法获取单实例锁：{error}"),
        ),
    };
    rfd::MessageDialog::new()
        .set_title("BiliArchive")
        .set_level(level)
        .set_description(message)
        .show();
    None
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};

    const ERROR_ALREADY_EXISTS: i32 = 183;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            attributes: *const c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn SetLastError(code: u32);
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    impl DesktopLock {
        pub(super) fn try_acquire() -> io::Result<Option<Self>> {
            let name: Vec<u16> = "Local\\BiliArchive.Desktop"
                .encode_utf16()
                .chain([0])
                .collect();
            // Retaining the handle keeps the named object alive for the lifetime of the desktop.
            // The mutex need not be owned by a thread.
            let (handle, status) = unsafe {
                SetLastError(0);
                let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
                (handle, io::Error::last_os_error())
            };
            let Some(handle) = NonNull::new(handle) else {
                return Err(status);
            };
            let lock = Self { handle };
            if status.raw_os_error() == Some(ERROR_ALREADY_EXISTS) {
                return Ok(None);
            }
            Ok(Some(lock))
        }
    }

    impl Drop for DesktopLock {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle.as_ptr()) };
        }
    }
}

#[cfg(not(windows))]
impl DesktopLock {
    fn try_acquire() -> io::Result<Option<Self>> {
        use std::fs::{self, OpenOptions, TryLockError};

        let dir = dirs::config_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "找不到应用配置目录"))?
            .join("BiliArchive");
        fs::create_dir_all(&dir)?;
        // Keep the path after exit so competing processes always lock the same file.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("desktop.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}
