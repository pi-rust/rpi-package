//! Bridge configuration: flag > environment > default.
//!
//! Mirrors the upstream `config.ts` precedence. The upstream `PI_OFFICE_BRIDGE_*`
//! names keep working so an existing shell setup needs no change; the native
//! `RPI_OFFICE_BRIDGE_*` names win when both are present.
#![forbid(unsafe_code)]

use crate::protocol::BRIDGE_DEFAULT_PORT;
use serde_json::Value;

/// Declared CLI flag for the listen port.
pub const FLAG_PORT: &str = "office-bridge-port";
/// Declared CLI flag for session-start autostart.
pub const FLAG_AUTOSTART: &str = "office-bridge-autostart";

const ENV_PORT: [&str; 2] = ["RPI_OFFICE_BRIDGE_PORT", "PI_OFFICE_BRIDGE_PORT"];
const ENV_AUTOSTART: [&str; 2] = ["RPI_OFFICE_BRIDGE_AUTOSTART", "PI_OFFICE_BRIDGE_AUTOSTART"];
const ENV_ALLOWED_ORIGINS: [&str; 2] = [
    "RPI_OFFICE_BRIDGE_ALLOWED_ORIGINS",
    "PI_OFFICE_BRIDGE_ALLOWED_ORIGINS",
];

/// Browser origins allowed to read the loopback `/health` surface.
///
/// The add-in task pane runs at these origins: the upstream dev Vite server and
/// the hosted GitHub Pages build. Extend with the allowed-origins env var when
/// the add-in is hosted elsewhere.
pub const DEFAULT_ALLOWED_ORIGINS: [&str; 3] = [
    "https://localhost:3141",
    "https://pi-excel.localhost",
    "https://dieuluucanh.github.io",
];

fn env_value(names: &[&str], env: &impl Fn(&str) -> Option<String>) -> Option<String> {
    for name in names {
        if let Some(value) = env(name) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn port_from_value(value: &Value) -> Option<u16> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port > 0),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            text.parse::<u16>().ok().filter(|port| *port > 0)
        }
        _ => None,
    }
}

/// Resolve the listen port: `--office-bridge-port` > env > default.
pub fn resolve_port(flag: Option<&Value>, env: &impl Fn(&str) -> Option<String>) -> u16 {
    if let Some(port) = flag.and_then(port_from_value) {
        return port;
    }
    if let Some(value) = env_value(&ENV_PORT, env) {
        if let Ok(port) = value.parse::<u16>() {
            if port > 0 {
                return port;
            }
        }
    }
    BRIDGE_DEFAULT_PORT
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(boolean) => *boolean,
        Value::String(text) => matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Value::Number(number) => number.as_f64().unwrap_or(0.0) != 0.0,
        _ => false,
    }
}

/// Resolve autostart: `--office-bridge-autostart` > env > false.
pub fn resolve_autostart(flag: Option<&Value>, env: &impl Fn(&str) -> Option<String>) -> bool {
    if let Some(value) = flag {
        return truthy(value);
    }
    match env_value(&ENV_AUTOSTART, env) {
        Some(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        None => false,
    }
}

/// Resolve the extra allowed `/health` origins from the environment.
pub fn extra_allowed_origins(env: &impl Fn(&str) -> Option<String>) -> Vec<String> {
    env_value(&ENV_ALLOWED_ORIGINS, env)
        .map(|raw| {
            raw.split(|c: char| c == ',' || c.is_whitespace())
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn port_precedence_is_flag_then_env_then_default() {
        let empty = env_of(&[]);
        assert_eq!(resolve_port(None, &empty), 38617);
        assert_eq!(
            resolve_port(Some(&json!(39000)), &empty),
            39000,
            "flag wins"
        );
        let upstream = env_of(&[("PI_OFFICE_BRIDGE_PORT", "39001")]);
        assert_eq!(resolve_port(None, &upstream), 39001);
        let native = env_of(&[
            ("PI_OFFICE_BRIDGE_PORT", "39001"),
            ("RPI_OFFICE_BRIDGE_PORT", "39002"),
        ]);
        assert_eq!(resolve_port(None, &native), 39002, "native env wins");
        assert_eq!(
            resolve_port(Some(&json!("39003")), &native),
            39003,
            "string flags parse"
        );
        let invalid = env_of(&[("PI_OFFICE_BRIDGE_PORT", "nope")]);
        assert_eq!(resolve_port(None, &invalid), 38617);
        assert_eq!(resolve_port(Some(&json!(0)), &empty), 38617);
    }

    #[test]
    fn autostart_defaults_off_and_accepts_flag_or_env() {
        let empty = env_of(&[]);
        assert!(!resolve_autostart(None, &empty));
        assert!(!resolve_autostart(Some(&json!(false)), &empty));
        assert!(resolve_autostart(Some(&json!(true)), &empty));
        assert!(resolve_autostart(
            None,
            &env_of(&[("PI_OFFICE_BRIDGE_AUTOSTART", "1")])
        ));
        assert!(resolve_autostart(
            None,
            &env_of(&[("RPI_OFFICE_BRIDGE_AUTOSTART", "yes")])
        ));
        assert!(!resolve_autostart(
            Some(&json!(false)),
            &env_of(&[("PI_OFFICE_BRIDGE_AUTOSTART", "1")])
        ));
    }

    #[test]
    fn extra_origins_are_comma_or_space_separated() {
        assert!(extra_allowed_origins(&env_of(&[])).is_empty());
        let origins = extra_allowed_origins(&env_of(&[(
            "PI_OFFICE_BRIDGE_ALLOWED_ORIGINS",
            "https://a.example, https://b.example  https://c.example",
        )]));
        assert_eq!(
            origins,
            vec![
                "https://a.example".to_string(),
                "https://b.example".to_string(),
                "https://c.example".to_string()
            ]
        );
    }
}
