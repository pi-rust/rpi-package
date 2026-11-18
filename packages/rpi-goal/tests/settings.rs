//! `goal.json` limits: defaults, invalid files, and the read-only view.
mod support;

use serde_json::json;

#[test]
fn a_missing_file_uses_the_documented_defaults() {
    let _guard = support::serial();
    let dir = support::SettingsDir::new(None);
    let settings = rpi_goal::settings::load();
    assert_eq!(settings.automatic_turns, Some(25));
    assert_eq!(settings.no_progress_turns, Some(3));
    assert!(settings.problem.is_none());
    assert!(
        !dir.dir.join("goal.json").exists(),
        "a missing file must not be created"
    );
}

#[test]
fn explicit_nulls_disable_the_limits() {
    let _guard = support::serial();
    let _dir = support::SettingsDir::new(Some(json!({
        "continuationLimits": {"automaticTurns": null, "noProgressTurns": null}
    })));
    let settings = rpi_goal::settings::load();
    assert_eq!(settings.automatic_turns, None);
    assert_eq!(settings.no_progress_turns, None);
    assert_eq!(settings.automatic_turns_label(), "Unlimited");
    assert_eq!(settings.no_progress_label(), "Off");
}

#[test]
fn an_invalid_file_falls_back_to_defaults_and_reports_read_only() {
    let _guard = support::serial();
    let _dir = support::SettingsDir::new(Some(json!({
        "continuationLimits": {"automaticTurns": "twenty five"}
    })));
    let settings = rpi_goal::settings::load();
    assert_eq!(settings.automatic_turns, Some(25));
    assert!(settings.problem.is_some());
    assert!(settings.read_only());
}

#[test]
fn malformed_json_is_reported_and_ignored() {
    let _guard = support::serial();
    let dir = support::SettingsDir::new(None);
    std::fs::write(dir.dir.join("goal.json"), "{not json").unwrap();
    let settings = rpi_goal::settings::load();
    assert_eq!(settings.automatic_turns, Some(25));
    assert!(settings.problem.unwrap().contains("not valid JSON"));
}

#[test]
fn the_settings_view_surfaces_the_effective_limits() {
    let _guard = support::serial();
    support::install();
    let _dir = support::SettingsDir::with_limits(Some(4), Some(2));
    let output = support::command("settings");
    let text = output["text"].as_str().unwrap();
    assert!(text.contains("automatic turns: 4"), "{text}");
    assert!(text.contains("no-progress turns: 2"), "{text}");
    assert!(text.contains("goal.json"), "{text}");
}

#[test]
fn the_agent_dir_override_must_be_absolute() {
    let _guard = support::serial();
    let previous = std::env::var_os("RPI_CODING_AGENT_DIR");
    std::env::set_var("RPI_CODING_AGENT_DIR", "relative/dir");
    let path = rpi_goal::settings::path();
    match previous {
        Some(previous) => std::env::set_var("RPI_CODING_AGENT_DIR", previous),
        None => std::env::remove_var("RPI_CODING_AGENT_DIR"),
    }
    // A relative override is ignored; the default agent dir is still used.
    if let Some(path) = path {
        assert!(path.is_absolute());
    }
}
