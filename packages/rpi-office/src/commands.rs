//! `/office` and `/office-tools` command surfaces.
//!
//! The grammar and help text are ported from the upstream `commands.ts`; the
//! help line and every status sentence are returned as command output so the
//! same strings are testable without a TUI.
#![forbid(unsafe_code)]

use crate::catalog::catalog;
use crate::lifecycle::{BridgeSnapshot, LifecycleState};

/// Parsed `/office` subcommand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfficeCommand {
    Status,
    Connect { port: Option<u16> },
    Close,
    Restart { port: Option<u16> },
    Help,
    Error(String),
}

const USAGE_STATUS: &str = "Usage: /office status";
const USAGE_CLOSE: &str = "Usage: /office close";

fn parse_port(raw: &str) -> Option<u16> {
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u16>().ok().filter(|port| *port > 0)
}

fn alias(raw: &str) -> &str {
    match raw {
        "on" | "start" => "connect",
        "stop" | "off" | "disconnect" => "close",
        "info" => "status",
        other => other,
    }
}

/// Parse the argument string after `/office`. Empty input means `status`.
pub fn parse_office_command(args: &str) -> OfficeCommand {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let Some(first) = tokens.first() else {
        return OfficeCommand::Status;
    };
    let lowered = first.to_lowercase();
    let name = alias(&lowered);
    match name {
        "status" => {
            if tokens.len() > 1 {
                OfficeCommand::Error(USAGE_STATUS.into())
            } else {
                OfficeCommand::Status
            }
        }
        "close" => {
            if tokens.len() > 1 {
                OfficeCommand::Error(USAGE_CLOSE.into())
            } else {
                OfficeCommand::Close
            }
        }
        "help" => OfficeCommand::Help,
        "connect" | "restart" => {
            if tokens.len() > 2 {
                return OfficeCommand::Error(format!("Usage: /office {name} [port]"));
            }
            let Some(token) = tokens.get(1) else {
                return if name == "connect" {
                    OfficeCommand::Connect { port: None }
                } else {
                    OfficeCommand::Restart { port: None }
                };
            };
            match parse_port(token) {
                Some(port) if name == "connect" => OfficeCommand::Connect { port: Some(port) },
                Some(port) => OfficeCommand::Restart { port: Some(port) },
                None => OfficeCommand::Error(format!(
                    "Invalid port \"{token}\": expected an integer from 1 to 65535."
                )),
            }
        }
        other => OfficeCommand::Error(format!(
            "Unknown subcommand \"{other}\". {}",
            office_command_help()
        )),
    }
}

/// The command help block, shared by `/office help` and parse errors.
pub fn office_command_help() -> String {
    [
        "Office bridge commands:",
        "  /office status                 show state, port and attached apps",
        "  /office connect [port]         start the bridge (default 38617)",
        "  /office close                  stop the bridge",
        "  /office restart [port]         stop, then start",
        "  /office help                   this list",
        "",
        "The bridge is off until you connect. Only one rpi session can own the port.",
    ]
    .join("\n")
}

/// Everything the status line needs, so rendering stays a pure function.
pub struct StatusInput<'a> {
    pub snapshot: &'a BridgeSnapshot,
    pub panes: &'a [crate::bridge::Pane],
    pub autostart: bool,
}

/// Multi-line `/office status` output.
pub fn status_text(input: &StatusInput<'_>) -> String {
    let snapshot = input.snapshot;
    match snapshot.state {
        LifecycleState::Off => {
            let mut lines = vec![format!(
                "Office bridge is off (port {}). Run /office connect to start it.",
                snapshot.port
            )];
            if let Some(error) = &snapshot.last_error {
                lines.push(format!("Last error: {error}"));
            }
            if input.autostart {
                lines.push("Autostart is enabled (--office-bridge-autostart).".into());
            }
            lines.join("\n")
        }
        LifecycleState::Starting => {
            format!("Office bridge is starting on :{}…", snapshot.port)
        }
        LifecycleState::Closing => "Office bridge is closing…".into(),
        LifecycleState::On => {
            let port = snapshot.actual_port.unwrap_or(snapshot.port);
            if input.panes.is_empty() {
                return format!(
                    "Office bridge is listening on :{port} — no app attached yet. (catalog v{})",
                    catalog().version
                );
            }
            let now = crate::protocol::now_ms();
            let lines: Vec<String> = input
                .panes
                .iter()
                .map(|pane| {
                    let model = pane
                        .model
                        .as_deref()
                        .map(|model| format!(", model={model}"))
                        .unwrap_or_default();
                    let provider = pane
                        .provider
                        .as_deref()
                        .map(|provider| format!(", provider={provider}"))
                        .unwrap_or_default();
                    let ago = now.saturating_sub(pane.last_seen) / 1000;
                    let ops = match &pane.ops {
                        None => "legacy client (v1 ops)".to_string(),
                        Some(ops) => match pane.catalog_version {
                            Some(version) => format!("{} ops (catalog v{version})", ops.len()),
                            None => format!("{} ops", ops.len()),
                        },
                    };
                    let ignored = if pane.ops_ignored > 0 {
                        format!(
                            ", {} advertised op(s) ignored (unknown to this server)",
                            pane.ops_ignored
                        )
                    } else {
                        String::new()
                    };
                    let pane_id: String = pane.pane_id.chars().take(8).collect();
                    format!(
                        "- {} · \"{}\"{} ({}, pane {}){}{}, seen {}s ago — {}{}",
                        pane.host.label(),
                        pane.document_label(),
                        if pane.focused { " (focused)" } else { "" },
                        pane.client_name,
                        pane_id,
                        model,
                        provider,
                        ago,
                        ops,
                        ignored
                    )
                })
                .collect();
            format!(
                "Office bridge on :{port} (catalog v{})\n{}",
                catalog().version,
                lines.join("\n")
            )
        }
    }
}

/// `/office-tools` output: every registered office tool and its active state.
pub fn tools_text(active: &[String]) -> String {
    let catalog = catalog();
    let mut names = catalog.tool_names.clone();
    names.push(crate::active_tools::OFFICE_LIST_TOOL.to_string());
    let mut lines: Vec<String> = Vec::new();
    for name in &names {
        let on = active.iter().any(|active| active == name);
        lines.push(format!("{} {name}", if on { "[on] " } else { "[off]" }));
    }
    format!(
        "Office tools ({} catalogue + discovery, catalog v{}):\n{}",
        catalog.tool_names.len(),
        catalog.version,
        lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_grammar_and_aliases() {
        assert_eq!(parse_office_command(""), OfficeCommand::Status);
        assert_eq!(parse_office_command("   "), OfficeCommand::Status);
        assert_eq!(parse_office_command("status"), OfficeCommand::Status);
        assert_eq!(parse_office_command("info"), OfficeCommand::Status);
        assert_eq!(
            parse_office_command("on"),
            OfficeCommand::Connect { port: None }
        );
        assert_eq!(
            parse_office_command("connect 39000"),
            OfficeCommand::Connect { port: Some(39000) }
        );
        assert_eq!(
            parse_office_command("start"),
            OfficeCommand::Connect { port: None }
        );
        assert_eq!(parse_office_command("stop"), OfficeCommand::Close);
        assert_eq!(parse_office_command("disconnect"), OfficeCommand::Close);
        assert_eq!(
            parse_office_command("restart 38618"),
            OfficeCommand::Restart { port: Some(38618) }
        );
        assert_eq!(parse_office_command("help"), OfficeCommand::Help);
    }

    #[test]
    fn rejects_bad_ports_and_extra_arguments() {
        assert_eq!(
            parse_office_command("connect 0"),
            OfficeCommand::Error("Invalid port \"0\": expected an integer from 1 to 65535.".into())
        );
        assert!(matches!(
            parse_office_command("connect 65536"),
            OfficeCommand::Error(_)
        ));
        assert!(matches!(
            parse_office_command("connect abc"),
            OfficeCommand::Error(_)
        ));
        assert_eq!(
            parse_office_command("status now"),
            OfficeCommand::Error("Usage: /office status".into())
        );
        match parse_office_command("nope") {
            OfficeCommand::Error(message) => {
                assert!(message.contains("Unknown subcommand \"nope\""));
                assert!(message.contains("/office connect [port]"));
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn status_text_covers_off_listening_and_attached() {
        let snapshot = BridgeSnapshot {
            state: LifecycleState::Off,
            port: 38617,
            actual_port: None,
            panes: 0,
            last_error: Some("port 38617 is already in use".into()),
            started_at: None,
        };
        let text = status_text(&StatusInput {
            snapshot: &snapshot,
            panes: &[],
            autostart: true,
        });
        assert!(text.contains("Office bridge is off (port 38617)"));
        assert!(text.contains("Last error: port 38617 is already in use"));
        assert!(text.contains("Autostart is enabled"));

        let snapshot = BridgeSnapshot {
            state: LifecycleState::On,
            port: 38617,
            actual_port: Some(39001),
            panes: 0,
            last_error: None,
            started_at: Some(1),
        };
        let text = status_text(&StatusInput {
            snapshot: &snapshot,
            panes: &[],
            autostart: false,
        });
        assert!(text.contains("listening on :39001"));
        assert!(text.contains("no app attached"));
        assert!(text.contains("catalog v3"));
    }

    #[test]
    fn tools_text_marks_active_state() {
        let text = tools_text(&[
            crate::active_tools::OFFICE_LIST_TOOL.to_string(),
            "office_excel_read_range".to_string(),
        ]);
        assert!(text.contains("[on]  office_list_documents"));
        assert!(text.contains("[on]  office_excel_read_range"));
        assert!(text.contains("[off] office_excel_charts"));
        assert!(text.contains("32 catalogue"));
    }
}
