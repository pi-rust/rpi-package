//! Safe platform lease primitive: noncreating open + nonblocking shared lock.
use std::{fs::File, io, path::Path};
#[cfg(all(test, any(unix, windows)))]
mod tests;
/// An observational fs2-compatible whole-file lease, not writer authorization.
/// Keep the owner through the protected decision. Drop closes its private,
/// synchronously-opened handle and releases the lock; no raw handle is exposed.
pub struct SharedFileLock {
    _file: File,
}
impl SharedFileLock {
    pub fn try_open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "shared lease requires a regular existing file",
            ));
        }
        platform_lock(&file, false)?;
        Ok(Self { _file: file })
    }
}
#[cfg(unix)]
fn platform_lock(file: &File, exclusive: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    extern "C" {
        fn flock(fd: std::os::raw::c_int, operation: std::os::raw::c_int) -> std::os::raw::c_int;
    }
    // SAFETY: valid borrowed descriptor, no pointer parameters; LOCK_SH/EX|NB.
    let result = unsafe { flock(file.as_raw_fd(), (if exclusive { 2 } else { 1 }) | 4) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
#[cfg(windows)]
fn platform_lock(file: &File, exclusive: bool) -> io::Result<()> {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            low: u32,
            high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    let mut overlapped = Overlapped {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        event: std::ptr::null_mut(),
    };
    // SAFETY: valid borrowed File handle, native OVERLAPPED layout. No
    // FILE_FLAG_OVERLAPPED on std's synchronous file, so the API cannot retain
    // the stack pointer. Same fs2 byte range; shared + FAIL_IMMEDIATELY (1).
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            1 | if exclusive { 2 } else { 0 },
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        Ok(())
    } else {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(33) {
            Err(io::Error::new(io::ErrorKind::WouldBlock, e))
        } else {
            Err(e)
        }
    }
}
#[cfg(not(any(unix, windows)))]
fn platform_lock(_file: &File, _exclusive: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native observation lock unsupported on this platform",
    ))
}
