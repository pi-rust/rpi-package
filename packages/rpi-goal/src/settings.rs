//! `~/.rpi/agent/goal.json` — the continuation safety limits.
//!
//! Mirrors pi-goal's `pi-goal.json` shape so the limits are recognizable:
//!
//! ```json
//! {"continuationLimits": {"automaticTurns": 25, "noProgressTurns": 3}}
//! ```
//!
//! A missing file uses the defaults without creating it. An invalid file also
//! uses the defaults but is reported as read-only, so the state is visible
//! instead of silently ignored. Writing the file is out of scope for 0.3.0.
use serde_json::Value;
use std::path::PathBuf;

pub const SETTINGS_FILE: &str = "goal.json";
pub const DEFAULT_AUTOMATIC_TURNS: u32 = 25;
pub const DEFAULT_NO_PROGRESS_TURNS: u32 = 3;

/// `None` means explicitly unlimited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub automatic_turns: Option<u32>,
    pub no_progress_turns: Option<u32>,
    /// Set when the file exists but could not be honored; the effective values
    /// are the defaults.
    pub problem: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            automatic_turns: Some(DEFAULT_AUTOMATIC_TURNS),
            no_progress_turns: Some(DEFAULT_NO_PROGRESS_TURNS),
            problem: None,
        }
    }
}

impl Settings {
    pub fn read_only(&self) -> bool {
        self.problem.is_some()
    }

    pub fn automatic_turns_label(&self) -> String {
        match self.automatic_turns {
            Some(n) => n.to_string(),
            None => "Unlimited".to_string(),
        }
    }

    pub fn no_progress_label(&self) -> String {
        match self.no_progress_turns {
            Some(n) => n.to_string(),
            None => "Off".to_string(),
        }
    }
}

/// `~/.rpi/agent`, overridable with `RPI_CODING_AGENT_DIR` (the native
/// `config::agent_dir()` contract). A relative override is ignored.
pub fn agent_dir() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("RPI_CODING_AGENT_DIR") {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    Some(home.join(".rpi").join("agent"))
}

pub fn path() -> Option<PathBuf> {
    agent_dir().map(|dir| dir.join(SETTINGS_FILE))
}

pub fn load() -> Settings {
    let Some(path) = path() else {
        return Settings::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        // Missing, unreadable, or a directory: defaults, and no write.
        return Settings::default();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => match normalize(&value) {
            Some(settings) => settings,
            None => Settings {
                problem: Some(format!("{} is invalid; using defaults", path.display())),
                ..Settings::default()
            },
        },
        Err(error) => Settings {
            problem: Some(format!(
                "{} is not valid JSON ({error}); using defaults",
                path.display()
            )),
            ..Settings::default()
        },
    }
}

/// `continuationLimits.automaticTurns` / `.noProgressTurns`: a positive integer,
/// or `null` for explicitly unlimited. Anything else is invalid — a typo must
/// not silently disable a safety limit.
fn normalize(value: &Value) -> Option<Settings> {
    let object = value.as_object()?;
    let mut settings = Settings::default();
    if let Some(limits) = object.get("continuationLimits") {
        let limits = limits.as_object()?;
        if let Some(value) = limits.get("automaticTurns") {
            settings.automatic_turns = limit(value)?;
        }
        if let Some(value) = limits.get("noProgressTurns") {
            settings.no_progress_turns = limit(value)?;
        }
    }
    Some(settings)
}

fn limit(value: &Value) -> Option<Option<u32>> {
    if value.is_null() {
        return Some(None);
    }
    let n = value.as_u64()?;
    if n == 0 || n > u32::MAX as u64 {
        return None;
    }
    Some(Some(n as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_the_documented_limits() {
        let settings = Settings::default();
        assert_eq!(settings.automatic_turns, Some(25));
        assert_eq!(settings.no_progress_turns, Some(3));
        assert!(!settings.read_only());
    }

    #[test]
    fn explicit_null_disables_a_limit() {
        let settings = normalize(&json!({
            "continuationLimits": {"automaticTurns": null, "noProgressTurns": null}
        }))
        .unwrap();
        assert_eq!(settings.automatic_turns, None);
        assert_eq!(settings.no_progress_turns, None);
        assert_eq!(settings.automatic_turns_label(), "Unlimited");
        assert_eq!(settings.no_progress_label(), "Off");
    }

    #[test]
    fn partial_files_keep_the_other_default() {
        let settings = normalize(&json!({"continuationLimits": {"automaticTurns": 4}})).unwrap();
        assert_eq!(settings.automatic_turns, Some(4));
        assert_eq!(settings.no_progress_turns, Some(3));
    }

    #[test]
    fn invalid_values_are_rejected_instead_of_disabled() {
        assert!(normalize(&json!({"continuationLimits": {"automaticTurns": 0}})).is_none());
        assert!(normalize(&json!({"continuationLimits": {"automaticTurns": -1}})).is_none());
        assert!(normalize(&json!({"continuationLimits": {"automaticTurns": "25"}})).is_none());
        assert!(normalize(&json!({"continuationLimits": {"noProgressTurns": true}})).is_none());
        assert!(normalize(&json!({"continuationLimits": []})).is_none());
        assert!(normalize(&json!("nope")).is_none());
    }

    #[test]
    fn unknown_fields_are_preserved_by_tolerance() {
        let settings = normalize(&json!({
            "retryUsageLimits": false,
            "continuationLimits": {"automaticTurns": 7},
            "future": {"x": 1}
        }))
        .unwrap();
        assert_eq!(settings.automatic_turns, Some(7));
    }

    #[test]
    fn bad_json_reports_read_only() {
        let settings = Settings {
            problem: Some("broken".into()),
            ..Settings::default()
        };
        assert!(settings.read_only());
        assert_eq!(settings.automatic_turns, Some(25));
    }
}
