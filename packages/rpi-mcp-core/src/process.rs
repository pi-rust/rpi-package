//! ABI-free process-tree lifetime owner using safe process-wrap APIs.
//! Windows JobObject spawns suspended, assigns the kill-on-close job, then
//! resumes threads. Unix uses a dedicated process group. Never check direct
//! child liveness before killing: descendants may still own inherited pipes.
use process_wrap::std::{StdChildWrapper, StdCommandWrap};
use std::process::Command;

#[derive(Debug)]
pub struct ProcessTree {
    child: Box<dyn StdChildWrapper>,
    stopped: bool,
}
impl ProcessTree {
    pub fn spawn(command: Command) -> Result<Self, String> {
        let mut command = StdCommandWrap::from(command);
        #[cfg(windows)]
        command.wrap(process_wrap::std::JobObject);
        #[cfg(unix)]
        command.wrap(process_wrap::std::ProcessGroup::leader());
        let child = command
            .spawn()
            .map_err(|e| format!("start MCP process tree: {e}"))?;
        Ok(Self {
            child,
            stopped: false,
        })
    }
    pub fn child_mut(&mut self) -> &mut std::process::Child {
        self.child.inner_mut()
    }
    pub fn kill(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        let _ = self.child.start_kill();
        let _ = self.child.inner_mut().kill();
        let _ = self.child.inner_mut().wait();
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.kill();
    }
}
