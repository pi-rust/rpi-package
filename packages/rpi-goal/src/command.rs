//! `/goal` slash command.
//!
//! Every mutating route persists state and then hands the work to the
//! continuation worker, so the command returns immediately and the first work
//! message is not sent from the TUI thread.
use serde_json::{json, Value};

use crate::ops;
use crate::render;
use crate::runtime;
use crate::state::{self, parse_duration_ms, parse_token_budget};

const COMMAND_HELP: &str = "\
/goal                      show status
/goal <objective>          start goal mode (shorthand for start)
/goal start <objective>    start goal mode (--notes \"...\", --tokens 100k)
/goal pause | resume       pause / resume the active goal
/goal stop                 complete and archive the goal (aliases: done, complete)
/goal clear                clear the goal and any leftover legacy file
/goal turns <N>            max turns (0 clears)
/goal time <dur>           max duration, e.g. 30m / 2h / 90s / 1h30m (0 clears)
/goal tokens <N>           token budget, e.g. 100k / 1.5m (0 clears)
/goal limit <N> [<dur>]    set turn and/or time budget
/goal list                 list active + archived goals
/goal settings             show the continuation limits (read-only)
/goal help                 usage";

/// `--notes "..."` and `--tokens 100k` extraction for the start route.
#[derive(Default)]
struct StartArgs {
    title: String,
    notes: Option<String>,
    tokens: Option<u64>,
}

fn parse_start_args(rest: &str) -> Result<StartArgs, String> {
    let mut title_parts: Vec<String> = Vec::new();
    let mut notes: Option<String> = None;
    let mut tokens: Option<u64> = None;
    let mut seen_start = false;
    let mut iter = tokenize(rest).into_iter().peekable();
    while let Some(token) = iter.next() {
        match token.as_str() {
            "--notes" | "-n" => {
                let mut collected = Vec::new();
                while let Some(next) = iter.peek() {
                    if next.starts_with("--") {
                        break;
                    }
                    collected.push(unquote(&iter.next().unwrap()));
                }
                notes = Some(collected.join(" "));
            }
            "--tokens" => {
                let Some(next) = iter.next() else {
                    return Err("--tokens requires a value such as 100k".into());
                };
                tokens = Some(parse_token_budget(&next)?);
            }
            other => {
                if !seen_start && other == "start" && title_parts.is_empty() {
                    seen_start = true;
                    continue;
                }
                title_parts.push(unquote(other));
            }
        }
    }
    Ok(StartArgs {
        title: title_parts.join(" "),
        notes,
        tokens,
    })
}

/// Split on whitespace while keeping `"quoted runs"` together, so
/// `/goal "ship it" --notes "be careful"` reads like a shell command.
fn tokenize(rest: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in rest.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Strip one matching pair of surrounding double quotes so
/// `/goal \"ship it\" --notes \"be careful\"` reads like a shell command.
fn unquote(token: &str) -> String {
    let trimmed = token.trim();
    let stripped = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        });
    stripped.unwrap_or(trimmed).to_string()
}

fn start(args: StartArgs) -> String {
    let token_budget = args.tokens;
    match ops::start(ops::StartOptions {
        title: args.title,
        notes: args.notes.unwrap_or_default(),
        max_turns: None,
        deadline_ms: None,
        token_budget,
    }) {
        Ok(goal) => format!(
            "🎯 Goal started: {}\n{}\nℹ️ Work starts automatically and continues until the goal completes, blocks, waits, or reaches a limit.",
            goal.title,
            goal.budget_summary(state::now_ms())
        ),
        Err(error) => format!("⚠️ {error}"),
    }
}

/// Route one `/goal` invocation. Returns the text the TUI renders.
pub fn run(args: &str) -> String {
    let args = args.trim();
    let (head, rest) = match args.split_once(char::is_whitespace) {
        Some((head, rest)) => (head.to_lowercase(), rest.trim()),
        None => (args.to_lowercase(), ""),
    };

    let result: Result<String, String> = match head.as_str() {
        "" | "status" | "show" => state::read().map(|goals| render::status_text(&goals)),
        "list" => state::read().map(|goals| render::list_text(&goals)),
        "help" => Ok(COMMAND_HELP.to_string()),
        "settings" => Ok(render::settings_text()),
        "start" => parse_start_args(rest).map(start),
        "pause" => ops::pause().map(|goal| {
            format!(
                "⏸️ Paused: {}\nNo further automatic turns will start. Resume with `/goal resume`.",
                goal.title
            )
        }),
        "resume" => ops::resume().map(|goal| format!("▶️ Resumed: {}", goal.title)),
        "stop" | "done" | "complete" => {
            ops::complete().map(|goal| format!("✅ Completed: {}", goal.title))
        }
        "clear" => ops::clear().map(|goal| match goal {
            Some(goal) => format!("🧹 Cleared: {}", goal.title),
            None => "🧹 No goal to clear.".to_string(),
        }),
        "turns" => match rest.parse::<u32>() {
            Ok(0) => ops::set_limits(Some(None), None, None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
            Ok(n) => ops::set_limits(Some(Some(n)), None, None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
            Err(_) => Err(format!("`{rest}` is not a turn count")),
        },
        "time" => match parse_duration_ms(rest) {
            Ok(0) => ops::set_limits(None, Some(None), None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
            Ok(ms) => ops::set_limits(None, Some(Some(ms)), None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
            Err(error) => Err(error),
        },
        "tokens" if rest.trim() == "0" => ops::set_limits(None, None, Some(None))
            .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
        "tokens" => match parse_token_budget(rest) {
            Ok(0) => ops::set_limits(None, None, Some(None))
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
            Ok(n) => ops::set_limits(None, None, Some(Some(n)))
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms()))),
            Err(error) => Err(error),
        },
        "limit" => {
            let mut parts = rest.split_whitespace();
            let turns = parts.next();
            let time = parts.next();
            if turns.is_none() && time.is_none() {
                return "Usage: /goal limit <turns> [duration]".to_string();
            }
            let max_turns = match turns {
                Some(token) => match token.parse::<u32>() {
                    Ok(0) => Some(None),
                    Ok(n) => Some(Some(n)),
                    Err(_) => return format!("`{token}` is not a turn count"),
                },
                None => None,
            };
            let deadline = match time {
                Some(token) => match parse_duration_ms(token) {
                    Ok(0) => Some(None),
                    Ok(ms) => Some(Some(ms)),
                    Err(error) => return error,
                },
                None => None,
            };
            ops::set_limits(max_turns, deadline, None)
                .map(|goal| format!("Budget updated: {}", goal.budget_summary(state::now_ms())))
        }
        // Unknown verb → treat the whole input as the objective (shorthand start).
        _ => parse_start_args(args).map(start),
    };

    match result {
        Ok(text) => text,
        Err(error) => format!("⚠️ {error}"),
    }
}

fn command_output(out: &mut rpi_plugin_sdk::callbacks::Output<'_>, value: Value) -> i32 {
    if !out.is_available() {
        return 1;
    }
    out.write(value.to_string());
    0
}

rpi_plugin_sdk::string_handler! {
fn goal_command(args_json, out, user_data) {
    let _scope = runtime::enter(runtime::callback(user_data));
    let envelope: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    let args = envelope
        .get("args")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let text = run(args);
    // The command runs outside any tool batch, so this boundary is safe.
    crate::state::flush();
    crate::inject::flush();
    command_output(out, json!({"kind": "message", "text": text}))
}
}

pub fn handler() -> rpi_plugin_sdk::CommandHandlerFn {
    goal_command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_args_split_title_notes_and_tokens() {
        let args =
            parse_start_args("Ship the release --notes \"be careful\" --tokens 100k").unwrap();
        assert_eq!(args.title, "Ship the release");
        assert_eq!(args.notes.as_deref(), Some("be careful"));
        assert_eq!(args.tokens, Some(100_000));
    }

    #[test]
    fn explicit_start_verb_is_not_part_of_the_objective() {
        let args = parse_start_args("start Ship it").unwrap();
        assert_eq!(args.title, "Ship it");
    }

    #[test]
    fn tokens_flag_requires_a_valid_value() {
        assert!(parse_start_args("x --tokens").is_err());
        assert!(parse_start_args("x --tokens nope").is_err());
    }

    #[test]
    fn help_and_settings_are_always_available() {
        assert!(run("help").contains("/goal start"));
        assert!(run("settings").contains("automatic turns"));
    }

    #[test]
    fn status_without_a_goal_is_explicit() {
        // No host runtime in unit tests: the file fallback returns no goal.
        let text = run("status");
        assert!(text.contains("No active goal"), "{text}");
    }
}
