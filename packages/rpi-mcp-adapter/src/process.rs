//! Own a process tree so inherited pipes cannot outlive the transport.
use std::process::{Child, Command};

#[cfg(windows)]
#[derive(Debug)]
pub struct ProcessTree(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
// SAFETY: an owned job HANDLE may be used and closed from any thread.
unsafe impl Send for ProcessTree {}

#[cfg(windows)]
impl ProcessTree {
    pub fn prepare(command: &mut Command) {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        // Assign the job before server code can spawn descendants or inherit pipes.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    pub fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let tree = Self(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
            {
                return Err(format!(
                    "protect MCP process tree: {}",
                    std::io::Error::last_os_error()
                ));
            }
            resume_child(child.id())?;
            Ok(tree)
        }
    }
    pub fn kill(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
pub struct ProcessTree(i32);
#[cfg(unix)]
impl ProcessTree {
    pub fn prepare(command: &mut Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    pub fn attach(child: &Child) -> Result<Self, String> {
        Ok(Self(child.id() as i32))
    }
    pub fn kill(&self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
fn resume_child(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "snapshot MCP thread: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut found = Thread32First(snapshot, &mut entry);
        let mut result = Err("MCP primary thread not found".into());
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread.is_null() {
                    result = Err(format!(
                        "open MCP thread: {}",
                        std::io::Error::last_os_error()
                    ));
                } else {
                    let resumed = ResumeThread(thread);
                    result = if resumed == u32::MAX {
                        Err(format!(
                            "resume MCP thread: {}",
                            std::io::Error::last_os_error()
                        ))
                    } else {
                        Ok(())
                    };
                    CloseHandle(thread);
                }
                break;
            }
            found = Thread32Next(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        result
    }
}
