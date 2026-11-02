//! Owned process-tree lifetime; no SDK or plugin ABI dependency.
//!
//! Windows job setup/thread resume and Unix process-group setup are adapted
//! from rpi-plugin-sdk 0.3.20, src/process.rs (BigFish), MIT:
//! Copyright (c) 2026 BigFish.
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
//! THE SOFTWARE.
//!
//! Unlike a bare Child or a fallible consuming wrapper, this owner is installed
//! immediately after spawn, before any fallible job/pipe setup. All raw platform
//! calls are confined to this module; every acquired Windows handle has RAII.
use std::io;
use std::process::{Child, Command};

#[derive(Debug)]
pub(crate) struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(unix)]
    group: i32,
    stopped: bool,
}

impl ProcessTree {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Self::spawn_with(command, |_, _| Ok(()))
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let child = command.spawn()?;
            Ok(Self {
                #[cfg(unix)]
                group: child.id() as i32,
                child,
                stopped: false,
            })
        }
    }

    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn stop(&mut self) {
        if self.stopped {
            return;
        }
        // Do not gate tree termination on Child::try_wait: a dead leader can
        // leave live descendants holding all transport pipes. The flag also
        // prevents a later Drop from signaling a potentially recycled Unix PGID.
        self.stopped = true;
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: job is owned and valid throughout the call. Closing it
            // afterwards is a second kill-on-close safeguard, even on failure.
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(job.as_raw_handle(), 1);
            }
        }
        #[cfg(unix)]
        // SAFETY: spawn created this dedicated group; kill takes only scalar
        // IDs/signals and accesses no Rust memory. Signal even after leader exit.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        // Also handles Windows errors before job assignment and tree-kill
        // failures. Child::kill/::wait retain the actual process handle/PID.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
const CREATION_FLAGS: u32 = windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
    | windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

// Local callback boundaries permit deterministic post-spawn fault injection,
// without global/environment failure switches or changing protocol behavior.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupStage {
    Spawned,
    JobCreated,
    Limited,
    Assigned,
}

#[cfg(windows)]
impl ProcessTree {
    fn spawn_with(
        command: &mut Command,
        mut checkpoint: impl FnMut(SetupStage, &Child) -> io::Result<()>,
    ) -> io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::JobObjects::*;

        // No server code can run before containment, and GUI hosts retain the
        // baseline headless launch behavior (pipes alone do not suppress windows).
        command.creation_flags(CREATION_FLAGS);
        let child = command.spawn()?;
        let mut owner = Self {
            child,
            job: None,
            stopped: false,
        };
        checkpoint(SetupStage::Spawned, &owner.child)?;
        // SAFETY: null security/name pointers request an unnamed, non-inherited
        // job. A non-null result transfers exactly one handle into OwnedHandle.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh valid owned job handle.
        owner.job = Some(unsafe { OwnedHandle::from_raw_handle(job) });
        checkpoint(SetupStage::JobCreated, &owner.child)?;
        // SAFETY: this Windows POD structure has a valid all-zero representation.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: owner keeps job alive; limits points to an initialized struct
        // of the exact size expected by this information class.
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        checkpoint(SetupStage::Limited, &owner.child)?;
        // SAFETY: both job and actual suspended child handles remain owned/live.
        // Kill-on-close was set BEFORE assignment; no unprotected running gap.
        if unsafe { AssignProcessToJobObject(job, owner.child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        checkpoint(SetupStage::Assigned, &owner.child)?;
        resume_child(owner.child.id())?;
        Ok(owner)
    }
}

#[cfg(windows)]
fn resume_child(pid: u32) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    // SAFETY: snapshot flags/PID are scalar OS values. A successful snapshot
    // transfers one handle to RAII; failure's sentinel is never adopted.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the snapshot is a fresh valid owned handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // SAFETY: THREADENTRY32 is Windows POD with a valid zero representation.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    // SAFETY: the owned snapshot is valid; entry is writable and size-initialized.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while found != 0 {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: scalar access flags and the enumerated live thread ID;
            // a successful result transfers a handle, not the thread lifetime.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenThread returned a fresh valid owned handle.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // SAFETY: thread remains owned/live for the call.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: same valid snapshot and writable initialized entry as above.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::other("MCP primary thread not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct Canary(PathBuf);
    impl Canary {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            Self(std::env::temp_dir().join(format!(
                "mcp-process-canary-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn command(&self) -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "process::tests::child_canary", "--nocapture"])
                .env("RPI_MCP_PROCESS_TEST_CANARY", &self.0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        }
    }
    impl Drop for Canary {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn child_canary() {
        if let Some(path) = std::env::var_os("RPI_MCP_PROCESS_TEST_CANARY") {
            std::fs::write(path, "server code executed").unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    fn wait_started(canary: &Canary) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !canary.0.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not execute startup canary"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(windows)]
    struct Probe(std::os::windows::io::OwnedHandle);
    #[cfg(windows)]
    impl Probe {
        fn new(child: &Child) -> Self {
            use std::os::windows::io::AsHandle;
            Self(child.as_handle().try_clone_to_owned().unwrap())
        }
        fn assert_exited(&self) {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
            // SAFETY: the duplicate process handle stays owned for both calls;
            // code is a writable local. Wait observes the actual process, not PID reuse.
            unsafe {
                assert_eq!(WaitForSingleObject(self.0.as_raw_handle(), 5000), 0);
                let mut code = 0;
                assert_ne!(GetExitCodeProcess(self.0.as_raw_handle(), &mut code), 0);
                assert_ne!(code, 259); // STILL_ACTIVE
            }
        }
    }
    #[cfg(unix)]
    struct Probe(u32);
    #[cfg(unix)]
    impl Probe {
        fn new(child: &Child) -> Self {
            Self(child.id())
        }
        fn assert_exited(&self) {
            // SAFETY: waitpid receives a scalar child PID and null status; no
            // Rust memory is exposed. ECHILD proves the owner already reaped it.
            assert_eq!(
                unsafe { libc::waitpid(self.0 as i32, std::ptr::null_mut(), libc::WNOHANG) },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }

    #[test]
    fn owner_drop_kills_and_waits_real_child() {
        let canary = Canary::new();
        let mut owner = ProcessTree::spawn(&mut canary.command()).unwrap();
        let probe = Probe::new(owner.child_mut());
        wait_started(&canary);
        drop(owner);
        probe.assert_exited();
    }

    #[test]
    fn post_spawn_pipe_setup_error_kills_and_waits_real_child() {
        let canary = Canary::new();
        let mut probe = None;
        let result = (|| -> io::Result<()> {
            // stdout is deliberately not piped: exercise the same fallible
            // extraction as transport, while the newly spawned owner is live.
            let mut owner = ProcessTree::spawn(&mut canary.command())?;
            probe = Some(Probe::new(owner.child_mut()));
            wait_started(&canary);
            let _stdout = owner
                .child_mut()
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("stdout unavailable"))?;
            Ok(())
        })();
        assert!(result.is_err());
        probe.unwrap().assert_exited();
    }

    #[test]
    fn stop_is_idempotent_and_reaps_real_child() {
        let canary = Canary::new();
        let mut owner = ProcessTree::spawn(&mut canary.command()).unwrap();
        let probe = Probe::new(owner.child_mut());
        wait_started(&canary);
        owner.stop();
        assert!(owner.stopped);
        assert!(owner.child_mut().try_wait().unwrap().is_some());
        owner.stop();
        drop(owner);
        probe.assert_exited();
    }

    #[cfg(windows)]
    #[test]
    fn headless_and_suspended_creation_flags_are_composed() {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        assert_eq!(CREATION_FLAGS, CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    #[cfg(windows)]
    #[test]
    fn failed_job_create_limit_assign_or_resume_cleans_suspended_child() {
        for stage in [
            SetupStage::Spawned,
            SetupStage::JobCreated,
            SetupStage::Limited,
            SetupStage::Assigned,
        ] {
            let canary = Canary::new();
            assert!(!canary.0.exists());
            let mut probe = None;
            let mut pid = None;
            let result = ProcessTree::spawn_with(&mut canary.command(), |at, child| {
                if at == SetupStage::Spawned {
                    pid = Some(child.id());
                    probe = Some(Probe::new(child));
                }
                if at == stage {
                    Err(io::Error::other(format!(
                        "injected failure before next phase: {stage:?}"
                    )))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err(), "stage {stage:?}, pid {pid:?}");
            probe
                .expect("actual child spawned before injection")
                .assert_exited();
            assert!(
                !canary.0.exists(),
                "suspended child executed at {stage:?}, pid {pid:?}"
            );
        }
    }
}
