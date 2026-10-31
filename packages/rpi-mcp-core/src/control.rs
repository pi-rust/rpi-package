use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct Control(pub Arc<AtomicBool>, pub Option<Arc<AtomicBool>>);

impl Control {
    pub fn linked(&self, other: &Control) -> Self {
        Self(self.0.clone(), Some(other.0.clone()))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<(), String> {
        if self.0.load(Ordering::Acquire)
            || self
                .1
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            Err("MCP request cancelled".into())
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        loop {
            if self.check().is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

pub struct Deadline {
    pub end: Instant,
    pub seconds: u64,
    pub method: String,
    pub control: Control,
}

impl Deadline {
    pub fn new(seconds: u64, method: &str, control: &Control) -> Self {
        let seconds = seconds.clamp(1, 300);
        Self {
            end: Instant::now() + Duration::from_secs(seconds),
            seconds,
            method: method.into(),
            control: control.clone(),
        }
    }
    pub fn check(&self) -> Result<(), String> {
        self.control.check()?;
        if Instant::now() >= self.end {
            return Err(format!(
                "MCP request {} timed out after {} seconds",
                self.method, self.seconds
            ));
        }
        Ok(())
    }
    pub fn slice(&self) -> Result<Duration, String> {
        self.check()?;
        Ok(self
            .end
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(10)))
    }
}
