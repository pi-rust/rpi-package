//! Rendering: plain text for `/goal`, Markdown envelopes for tools.
use serde_json::{json, Value};

use crate::settings;
use crate::state::{self, Goal, Goals, Status};

fn notes_or_placeholder(goal: &Goal, placeholder: &str) -> String {
    let notes = goal.notes.trim();
    if notes.is_empty() {
        placeholder.to_string()
    } else {
        notes.to_string()
    }
}

fn status_line(goal: &Goal) -> String {
    match goal.waiting.as_ref() {
        Some(waiting) => format!("{} (waiting: {})", goal.status.as_str(), waiting.reason),
        None => {
            let cause = goal
                .safety_pause_cause
                .as_deref()
                .map(|cause| format!(", {cause}"))
                .unwrap_or_default();
            format!("{}{cause}", goal.status.as_str())
        }
    }
}

/// `/goal` / `/goal status`.
pub fn status_text(goals: &Goals) -> String {
    let now = state::now_ms();
    let Some(goal) = goals.active.as_ref() else {
        return "🎯 No active goal. Start one with `/goal <objective>`.".to_string();
    };
    format!(
        "🎯 {}\nstatus: {}\nid: {}\nnotes: {}\n{}",
        goal.title,
        status_line(goal),
        goal.id,
        notes_or_placeholder(goal, "(no notes)"),
        goal.budget_summary(now)
    )
}

/// `/goal list`.
pub fn list_text(goals: &Goals) -> String {
    let now = state::now_ms();
    let mut lines = vec!["🎯 Goals".to_string()];
    match goals.active.as_ref() {
        Some(goal) => lines.push(format!(
            "• {} ({}) — {}",
            goal.title,
            status_line(goal),
            goal.budget_summary(now)
        )),
        None => lines.push("• (no active goal)".to_string()),
    }
    if !goals.archived.is_empty() {
        lines.push(format!("archived: {}", goals.archived.len()));
        for goal in goals.archived.iter().rev().take(10) {
            lines.push(format!("  – {} ({})", goal.title, goal.status.as_str()));
        }
    }
    lines.join("\n")
}

/// `/goal settings`: read-only view of the continuation limits.
pub fn settings_text() -> String {
    let settings = settings::load();
    let path = settings::path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(no home directory)".to_string());
    let mut lines = vec![
        format!("⚙️ Goal settings — {path}"),
        format!("automatic turns: {}", settings.automatic_turns_label()),
        format!("no-progress turns: {}", settings.no_progress_label()),
    ];
    match settings.problem.as_deref() {
        Some(problem) => {
            lines.push(format!("⚠️ {problem}"));
            lines.push("Editing is required: this view is read-only.".to_string());
        }
        None => {
            lines.push("Editing the file applies at the next session start or /reload.".to_string())
        }
    }
    lines.join("\n")
}

fn goal_summary_json(goal: &Goal) -> Value {
    json!({
        "id": goal.id,
        "title": goal.title,
        "status": goal.status.as_str(),
        "notes": goal.notes,
        "iteration": goal.iteration,
        "turnsUsed": goal.turns_used,
        "maxTurns": goal.max_turns,
        "deadlineMs": goal.deadline_ms,
        "tokenBudget": goal.token_budget,
        "tokensUsed": goal.tokens_used,
        "automaticTurns": goal.automatic_turns,
        "noProgressCount": goal.no_progress_count,
        "safetyPauseCause": goal.safety_pause_cause,
        "waiting": goal.waiting.as_ref().map(|waiting| json!({
            "reason": waiting.reason,
            "kind": format!("{:?}", waiting.kind).to_lowercase(),
            "deadlineMs": waiting.deadline_ms,
        })),
        "startedAt": goal.started_at,
        "updatedAt": goal.updated_at,
    })
}

/// The tool result envelope. `expandable` reflects that a goal is still live.
pub fn envelope(action: &str, text: String, active: bool, goal: Option<&Goal>) -> String {
    json!({
        "content": [{"type": "text", "text": text}],
        "details": {
            "kind": "goal",
            "action": action,
            "active": active,
            "markdown": true,
            "goal": goal.map(goal_summary_json),
        }
    })
    .to_string()
}

pub fn show_markdown(goal: Option<&Goal>) -> String {
    match goal {
        Some(goal) => format!(
            "## {} Current goal\n\n**{}**  ({})\n\n{}\n\n{}\n\n`goal_id`: `{}`",
            icon(goal.status),
            goal.title,
            status_line(goal),
            notes_or_placeholder(goal, "_no notes_"),
            goal.budget_summary(state::now_ms()),
            goal.id
        ),
        None => "## 🎯 Goal\n\n_No active goal._ Start one with `goal start`.".to_string(),
    }
}

pub fn list_markdown(goals: &Goals) -> String {
    let mut lines = vec!["## 🎯 Goals".to_string()];
    match goals.active.as_ref() {
        Some(goal) => lines.push(format!(
            "- **{}** ({}): {}",
            goal.title,
            status_line(goal),
            notes_or_placeholder(goal, "_no notes_")
        )),
        None => lines.push("_No active goal._".to_string()),
    }
    if !goals.archived.is_empty() {
        lines.push(String::new());
        lines.push("### Archived".to_string());
        for goal in goals.archived.iter().rev() {
            lines.push(format!("- ~~{}~~ ({})", goal.title, goal.status.as_str()));
        }
    }
    lines.join("\n")
}

pub fn started_markdown(goal: &Goal) -> String {
    format!(
        "## 🎯 Goal started\n\n**{}**\n\n{}\n\n{}\n\nGoal mode is now active; automatic continuation is enabled until the goal completes.",
        goal.title,
        notes_or_placeholder(goal, "_no notes_"),
        goal.budget_summary(state::now_ms())
    )
}

pub fn updated_markdown(goal: &Goal) -> String {
    format!(
        "## 🎯 Goal updated\n\n**{}**  ({})\n\n{}\n\n{}",
        goal.title,
        status_line(goal),
        notes_or_placeholder(goal, "_no notes_"),
        goal.budget_summary(state::now_ms())
    )
}

pub fn limits_markdown(goal: &Goal) -> String {
    format!(
        "## 🎯 Goal budget updated\n\n**{}**\n\n{}",
        goal.title,
        goal.budget_summary(state::now_ms())
    )
}

pub fn completed_markdown(goal: &Goal) -> String {
    format!(
        "## ✅ Goal complete\n\n**{}**\n\nArchived. Automatic continuation has stopped.",
        goal.title
    )
}

fn icon(status: Status) -> &'static str {
    match status {
        Status::Active => "🎯",
        Status::Paused => "⏸️",
        Status::Blocked => "⛔",
        Status::Waiting => "⏳",
        Status::UsageLimited => "🔌",
        Status::BudgetLimited => "⛔",
        Status::Complete => "✅",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal() -> Goal {
        Goal::new("Ship it".into(), "notes".into(), 0)
    }

    #[test]
    fn empty_state_is_explicit() {
        let goals = Goals::default();
        assert!(status_text(&goals).contains("No active goal"));
        assert!(list_text(&goals).contains("no active goal"));
        assert!(show_markdown(None).contains("No active goal"));
        assert!(list_markdown(&goals).contains("No active goal"));
    }

    #[test]
    fn status_text_shows_id_and_budget() {
        let goal = goal();
        let goals = Goals {
            version: 2,
            active: Some(goal.clone()),
            archived: vec![],
        };
        let text = status_text(&goals);
        assert!(text.contains("Ship it"));
        assert!(text.contains(&goal.id));
        assert!(text.contains("turns 0 (unlimited)"));
    }

    #[test]
    fn envelope_carries_the_goal_and_markdown_flag() {
        let goal = goal();
        let value: Value =
            serde_json::from_str(&envelope("show", "text".into(), true, Some(&goal))).unwrap();
        assert_eq!(value["details"]["kind"], "goal");
        assert_eq!(value["details"]["markdown"], true);
        assert_eq!(value["details"]["goal"]["id"], goal.id);
        assert_eq!(value["content"][0]["text"], "text");
    }

    #[test]
    fn archived_goals_are_listed_last_first() {
        let mut first = goal();
        first.title = "first".into();
        let mut second = goal();
        second.title = "second".into();
        let goals = Goals {
            version: 2,
            active: None,
            archived: vec![first, second],
        };
        let text = list_text(&goals);
        let second_at = text.find("second").unwrap();
        let first_at = text.find("first").unwrap();
        assert!(second_at < first_at);
    }

    #[test]
    fn waiting_status_names_its_reason() {
        let mut goal = goal();
        goal.waiting = Some(state::Waiting {
            reason: "review monitor".into(),
            kind: state::WaitKind::External,
            deadline_ms: None,
        });
        let goals = Goals {
            version: 2,
            active: Some(goal),
            archived: vec![],
        };
        assert!(status_text(&goals).contains("review monitor"));
    }
}
