//! Parent-side process lifetime only. No workspace, HTTP server or Agent owner.
pub mod packages;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// Native launch composition. Private control credentials use the generated
/// stdin handoff, never arguments, environment or exported configuration.
pub struct Launch {
    pub executable: PathBuf,
    pub library: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub directory: Option<PathBuf>,
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub ready: bool,
    pub address: String,
    pub node_id: String,
    pub control_address: String,
}
/// Capability is intentionally neither Debug nor Serialize. Never pass it to a WebView.
pub struct Control {
    pub ready: Ready,
    capability: String,
}
impl Control {
    pub fn capability(&self) -> &str {
        &self.capability
    }
}

#[must_use = "Retain the native child until application shutdown"]
pub struct Process {
    child: Option<Child>,
    tree: Option<platform::Tree>,
}
impl Process {
    pub fn start(options: Launch) -> Result<(Self, Control), String> {
        let library = options
            .library
            .canonicalize()
            .map_err(|_| "Native node extension is unavailable")?;
        if !library.is_file() {
            return Err("Native node extension is not a file".into());
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| "Cannot create private node capability")?;
        let capability = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut command = Command::new(options.executable);
        command
            .args([
                "--local-only",
                "--no-session",
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
            ])
            .arg("--extension")
            .arg(library)
            .arg("--node-control-stdin")
            .args(options.arguments)
            .envs(options.environment)
            .env_remove("RPI_NODE_CONTROL_STDIN")
            .env_remove("RPI_NODE_CONTROL_TOKEN")
            .env_remove("RPI_EXTENSIONS_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = options.directory {
            command.current_dir(directory);
        }
        platform::configure(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| "Cannot launch native rpi host")?;
        let tree = match platform::Tree::attach(&child) {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stdout = child.stdout.take().ok_or("Missing native ready pipe")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("Missing native diagnostics pipe")?;
        let mut process = Self {
            child: Some(child),
            tree: Some(tree),
        };
        // A single small frame precedes the EOF supervision stream. Unlike an
        // environment secret, this capability cannot reach Agent grandchildren.
        let frame = serde_json::to_vec(&serde_json::json!({"capability":capability}))
            .map_err(|_| "Cannot encode private handoff")?;
        let stdin = process
            .child
            .as_mut()
            .and_then(|c| c.stdin.as_mut())
            .ok_or("Missing native control pipe")?;
        stdin
            .write_all(&frame)
            .and_then(|()| stdin.write_all(b"\n"))
            .map_err(|_| "Cannot hand off native control capability")?;
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("rpi-node-ready".into())
            .spawn(move || {
                let mut stdout = stdout;
                let result = read_ready(&mut stdout);
                let _ = send.send(result);
                // No unbounded buffers or blocking GUI writes after readiness.
                let _ = io::copy(&mut stdout, &mut io::sink());
            })
            .map_err(|_| "Cannot supervise native ready pipe")?;
        std::thread::Builder::new()
            .name("rpi-node-diagnostics".into())
            .spawn(move || {
                let _ = io::copy(&mut { stderr }, &mut io::sink());
            })
            .map_err(|_| "Cannot supervise native diagnostics")?;
        match receive.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(ready)) => Ok((process, Control { ready, capability })),
            _ => {
                let _ = process.stop();
                Err("Native node did not become ready; check extension installation and model-free rpi service support".into())
            }
        }
    }
    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
    /// EOF requests ordinary extension shutdown. The process tree is owned from
    /// launch, so timeout cleanup cannot target an unrelated process/listener.
    pub fn stop(&mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.tree.take();
                    return if status.success() {
                        Ok(())
                    } else {
                        Err("Native node exited with an error".into())
                    };
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    self.tree.take();
                    platform::terminate(&mut child);
                    let _ = child.wait();
                    return Err(
                        "Native node shutdown was not confirmed; owned process tree terminated"
                            .into(),
                    );
                }
            }
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn read_ready(input: &mut impl Read) -> Result<Ready, String> {
    let mut line = Vec::new();
    for byte in io::BufReader::new(input).take(64 * 1024).bytes() {
        let byte = byte.map_err(|_| "Cannot read native ready frame")?;
        if byte == b'\n' {
            if let Ok(ready) = serde_json::from_slice::<Ready>(&line) {
                if ready.ready
                    && !ready.node_id.is_empty()
                    && ready.node_id.len() <= 256
                    && ready.address.len() <= 2048
                    && ready.control_address.len() <= 2048
                {
                    return Ok(ready);
                }
            }
            line.clear();
        } else {
            if line.len() >= 16 * 1024 {
                return Err("Native ready frame exceeds limit".into());
            }
            line.push(byte);
        }
    }
    Err("Native ready frame missing or incomplete".into())
}

#[cfg(windows)]
mod platform {
    use std::{
        os::windows::io::AsRawHandle,
        os::windows::process::CommandExt,
        process::{Child, Command},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::CREATE_NO_WINDOW,
        },
    };
    pub fn configure(command: &mut Command) {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    pub struct Tree(HANDLE);
    // The owned job handle is never used concurrently, and may be closed by a
    // different application shutdown thread.
    unsafe impl Send for Tree {}
    impl Tree {
        pub fn attach(child: &Child) -> Result<Self, String> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err("Cannot create native node process job".into());
                }
                let job = Self(handle);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                    || AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) == 0
                {
                    return Err("Cannot supervise native node process tree".into());
                }
                Ok(job)
            }
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    pub fn terminate(child: &mut Child) {
        let _ = child.kill();
    }
}
#[cfg(unix)]
mod platform {
    use std::{
        os::unix::process::CommandExt,
        process::{Child, Command},
    };
    pub fn configure(command: &mut Command) {
        command.process_group(0);
    }
    pub struct Tree(u32);
    impl Tree {
        pub fn attach(child: &Child) -> Result<Self, String> {
            Ok(Self(child.id()))
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0 as i32), libc::SIGKILL);
            }
        }
    }
    pub fn terminate(child: &mut Child) {
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_bounded_credential_free_ready_frames_are_accepted() {
        let frame=b"notice\n{\"ready\":true,\"nodeId\":\"n\",\"address\":\"http://127.0.0.1:1/\",\"controlAddress\":\"http://127.0.0.1:2/\"}\n";
        assert_eq!(read_ready(&mut &frame[..]).unwrap().node_id, "n");
        assert!(read_ready(&mut &frame[..frame.len() - 1]).is_err());
        assert!(read_ready(&mut &vec![b'x'; 17 * 1024][..]).is_err());
    }
}
