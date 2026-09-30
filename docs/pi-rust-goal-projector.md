# pi-rust host change: project `goal` custom entries into context

**Goal:** make the `rpi-goal` extension's injected goal visible to the model on
every turn, by registering a `CustomEntryContextMessageProjector` for the
`"goal"` custom-entry type.

This is the host half of the "clean path". The plugin half is already done in
`rpi-package/packages/rpi-goal` (it appends `customType: "goal"` entries on
`TurnEnd`). Until the host registers the projector below, those entries are inert
(`build_session_context` drops custom entries with no projector).

## Background (verified against the current tree)

- `AgentHarnessOptions` has `entry_projectors: BTreeMap<String, CustomEntryContextMessageProjector>`.
- The production harness in `crates/rpi-cli/src/session.rs` sets
  `entry_projectors: Default::default()` — i.e. **no** projectors, so every
  extension custom entry is currently invisible to the model.
- `CustomEntryContextMessageProjector =
  Arc<dyn Fn(&CustomEntry, usize, &[Entry]) -> Vec<AgentMessage> + Send + Sync>`
  (`rpi_harness::session::context`, re-exported as
  `rpi_harness::session::CustomEntryContextMessageProjector`).
- `CustomEntry { base: EntryBase, custom_type: String, data: Option<serde_json::Value> }`
  (`rpi_harness::session::types`).
- A projector's returned `AgentMessage`s are spliced into `ctx.messages` and
  then passed through `convert_to_llm`. A `UserMessage` renders as user text.
- The plugin writes `data` as:
  ```json
  {"title":"…","status":"active|paused|complete","notes":"…","startedAt":<ms>,"updatedAt":<ms>}
  ```

## Required change

### 1. Add a projector helper

Put this next to the other harness helpers in `crates/rpi-cli/src/session.rs`
(or a small new module `goal_projector.rs` that `session.rs` imports):

```rust
use std::collections::BTreeMap;
use std::sync::Arc;

use rpi_agent::AgentMessage;
use rpi_ai::types::UserMessage;
use rpi_harness::session::types::Entry;
use rpi_harness::session::CustomEntryContextMessageProjector;

/// Project the latest `goal` custom entry into a user-visible message so the
/// model keeps seeing the active objective across turns.
///
/// Only the LAST `goal` entry in the branch path is projected, so a
/// `complete`/`pause` supersedes an earlier `active` entry. `complete` projects
/// nothing (the goal is done and must not keep steering the model).
fn project_goal_entry(
    entry: &rpi_harness::session::types::CustomEntry,
    _index: usize,
    entries: &[Entry],
) -> Vec<AgentMessage> {
    // Only the newest goal entry is authoritative.
    let is_latest = entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Custom(c) if c.custom_type == "goal" => Some(c.base.id.as_str()),
            _ => None,
        })
        .map(|id| id == entry.base.id)
        .unwrap_or(true);
    if !is_latest {
        return Vec::new();
    }

    let data = entry.data.as_ref();
    let get = |k: &str| data.and_then(|d| d.get(k)).and_then(|v| v.as_str());
    let status = get("status").unwrap_or("active");
    if status == "complete" {
        return Vec::new();
    }
    let title = get("title").unwrap_or("(untitled goal)");
    let notes = get("notes").unwrap_or("").trim();

    let tag = if status == "paused" {
        "⏸️ Paused goal"
    } else {
        "🎯 Active goal"
    };
    let text = if notes.is_empty() {
        format!("{tag}: {title}. Keep advancing this goal.")
    } else {
        format!("{tag}: {title} — {notes}. Keep advancing this goal.")
    };
    vec![AgentMessage::User(UserMessage::new(text, entry.base.timestamp))]
}

/// Build the `entry_projectors` map handed to `AgentHarnessOptions`.
pub fn goal_entry_projectors() -> BTreeMap<String, CustomEntryContextMessageProjector> {
    let mut map: BTreeMap<String, CustomEntryContextMessageProjector> = BTreeMap::new();
    map.insert("goal".to_string(), Arc::new(project_goal_entry));
    map
}
```

### 2. Wire it into the production harness

In `crates/rpi-cli/src/session.rs`, inside the `let options = AgentHarnessOptions { … }`
block (currently around line 645), replace:

```rust
        entry_projectors: Default::default(),
```

with:

```rust
        entry_projectors: goal_entry_projectors(),
```

That is the only production site. (There is a second site in
`crates/rpi-cli/src/agent_session.rs` inside `#[cfg(test)] fn test_harness()` —
leave it as `Default::default()`.)

### 3. (Optional) forward-compat

If you want a single place to add more extension-entry projectors later, keep
`goal_entry_projectors()` and merge additional projectors into the same map.

## Acceptance test

Add a unit test in `session.rs` (same module as the helper):

```rust
#[test]
fn goal_entry_projects_active_and_skips_complete() {
    use rpi_harness::session::types::{CustomEntry, EntryBase};
    let mk = |id: &str, status: &str| Entry::Custom(CustomEntry {
        base: EntryBase {
            entry_type: "custom".into(),
            id: id.into(),
            seq: 0,
            parent_id: None,
            timestamp: 1,
        },
        custom_type: "goal".into(),
        data: Some(serde_json::json!({"title":"T","status":status,"notes":"N"})),
    });

    let active = mk("g1", "active");
    let complete = mk("g2", "complete");

    // Latest is `active` → injected once with the goal text.
    let out = match &active { Entry::Custom(c) => project_goal_entry(c, 0, &[active.clone()]), _ => unreachable!() };
    assert_eq!(out.len(), 1);
    assert!(matches!(&out[0], AgentMessage::User(_)));

    // Latest is `complete` → nothing injected.
    let out = match &complete { Entry::Custom(c) => project_goal_entry(c, 1, &[active.clone(), complete.clone()]), _ => unreachable!() };
    assert!(out.is_empty());

    // An older `active` entry is not re-injected when a newer goal entry exists.
    let newer = mk("g3", "paused");
    let out = match &active { Entry::Custom(c) => project_goal_entry(c, 0, &[active.clone(), newer.clone()]), _ => unreachable!() };
    assert!(out.is_empty());
}
```

## How to verify end to end

1. `cargo test -p rpi-cli goal_entry`
2. `task install` in `rpi-package` (or copy the built `rpi_goal.dll` to
   `~/.rpi/agent/extensions/`).
3. Restart `rpi`, run `goal start "test" — the model should see
   `🎯 Active goal: test…` in the next turn; `goal complete` should stop it.
4. `.rpi/goals.json` records the goal; the transcript gains a `custom` entry
   with `customType: "goal"` per state change.

## Note on the plugin contract

The plugin appends a new `goal` custom entry on every state change
(start/update/pause/resume/complete), not on every turn. The projector must
therefore pick the **latest** entry (as above) — do not concatenate all of them,
or the goal would appear many times and a completed goal would linger.

## Files touched

- `crates/rpi-cli/src/session.rs` — add `goal_entry_projectors()` + test; change
  `entry_projectors: Default::default()` → `goal_entry_projectors()` in the
  `AgentHarnessOptions` literal (production site only).

No other crate changes are required; `rpi-harness` already supports projectors.
