#![forbid(unsafe_code)]
// The `export_plugin!` expansion exposes the raw-pointer ABI entrypoints the host
// calls. Every other plugin crate in this workspace carries the same allow (see
// rpi-acp, rpi-btw, rpi-subagents, rpi-ui-adapter); without it
// `cargo clippy -p rpi-orchestrator` fails on the macro expansion alone.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! `rpi-orchestrator` — DAG subagent orchestration for the rpi Rust agent.
//!
//! A subagent is a child `rpi` process. Three tools share one session-scoped
//! task graph:
//!
//! - `subagent` — run one bounded task (the delegation primitive),
//! - `plan`     — author/edit the DAG,
//! - `dispatch` — run the ready set (one wave per call).
//!
//! The graph lives in this process's memory for the life of a run. Switching
//! sessions clears it — a human decides whether to resume or start over, rather
//! than the extension silently replaying an old branch. `plan`/`dispatch`
//! results still carry the graph in `details` for inspection. Design and
//! stages: `DESIGN.md`.

mod control;
mod dispatch;
mod graph;
mod host;
mod kit;
mod plan;
mod spawn;
mod subagent;

use serde_json::Value;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The orchestration graph, scoped to the session the host says it is serving.
///
/// The plan is session state: switching sessions must not let session B see or
/// dispatch session A's plan. The host injects `__rpi.sessionId` into every
/// plugin tool call ([`rpi_plugin_sdk::HOST_CONTEXT_KEY`]), so the current
/// session is known without the model supplying it. A host that predates that
/// field — or an ephemeral session with no id — shares a single fallback graph
/// rather than refusing to work.
#[derive(Default)]
pub(crate) struct GraphState {
    session: Option<String>,
    graph: graph::Graph,
    generation: u64,
}

static GRAPH: OnceLock<Mutex<GraphState>> = OnceLock::new();

fn graph_state() -> &'static Mutex<GraphState> {
    GRAPH.get_or_init(|| Mutex::new(GraphState::default()))
}

/// The session id the host attached to this tool call, if any.
pub(crate) fn session_of(params: &Value) -> Option<String> {
    params
        .get(rpi_plugin_sdk::HOST_CONTEXT_KEY)
        .and_then(|ctx| ctx.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Lock the graph belonging to `params`' session.
///
/// A session change **resets** the graph: whether to resume an old plan or start
/// over is a human decision, not something the extension should silently replay.
/// A poisoned lock is recovered rather than panicking, mirroring the SDK's
/// tool-handle convention.
pub(crate) fn lock_graph(params: &Value) -> MutexGuard<'static, GraphState> {
    let session = session_of(params);
    let mut guard = graph_state().lock().unwrap_or_else(|p| p.into_inner());
    if guard.session != session {
        guard.session = session;
        guard.graph = graph::Graph::default();
        guard.advance_generation();
    }
    guard
}

/// Rejoin a wave without changing the active session or touching a newer plan.
/// The generation also detects A -> B -> A switches with reused node ids.
pub(crate) fn lock_graph_for_wave(
    params: &Value,
    generation: u64,
) -> Result<MutexGuard<'static, GraphState>, String> {
    let guard = graph_state().lock().unwrap_or_else(|p| p.into_inner());
    if guard.session != session_of(params) || guard.generation != generation {
        return Err(
            "plan or session changed while dispatch was running; stale results discarded".into(),
        );
    }
    Ok(guard)
}

impl GraphState {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn advance_generation(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("graph generation exhausted");
    }

    pub(crate) fn is_running(&self) -> bool {
        self.graph
            .nodes
            .iter()
            .any(|node| node.status == graph::Status::Running)
    }

    #[cfg(test)]
    pub(crate) fn graph(&self) -> &graph::Graph {
        &self.graph
    }

    pub(crate) fn graph_mut(&mut self) -> &mut graph::Graph {
        &mut self.graph
    }
}

crate::tool_entry!(subagent_tool, crate::subagent::subagent);
crate::tool_entry!(plan_tool, crate::plan::plan);
crate::tool_entry!(dispatch_tool, crate::dispatch::dispatch);

const SUBAGENT_PARAMS: &str = r#"{"type":"object","properties":{"task":{"type":"string","maxLength":20000},"role":{"type":"string"},"provider":{"type":"string","description":"Leave empty to run on this session's provider (recommended). Only set it to a provider you know is authenticated for this machine; an unknown or unauthenticated provider fails the delegation with 'No API key'."},"model":{"type":"string","description":"Leave empty to run on this session's model. Only set it together with a `provider`, or when the model id is unique across providers."},"maxTurns":{"type":"integer","minimum":1,"maximum":32},"timeoutSeconds":{"type":"integer","minimum":10,"maximum":1800}},"required":["task"]}"#;

const PLAN_PARAMS: &str = r#"{"type":"object","properties":{"action":{"type":"string","enum":["set","update","show"]},"nodes":{"type":"array","description":"action=set: full node list. Each node: {id, task, deps?, role?, provider?, model?, maxTurns?, timeoutSeconds?, expectFile?, verify?}"},"addNodes":{"type":"array","description":"action=update: nodes to add (same node shape)"},"removeIds":{"type":"array","items":{"type":"string"},"description":"action=update: ids to remove"},"setDeps":{"type":"array","description":"action=update: [{id, deps}]"},"reset":{"type":"array","items":{"type":"string"},"description":"action=update: re-arm these ids to pending for another attempt"}},"required":["action"]}"#;

const DISPATCH_PARAMS: &str = r#"{"type":"object","properties":{"maxParallel":{"type":"integer","minimum":1,"maximum":16,"description":"max concurrent subagents for this wave (default 4)"}}}"#;

/// Optional override for the orchestration guidance injected into the system
/// prompt. Guidance is **on by default whenever a goal is active** — `/goal`
/// implies orchestration, so no variable is needed. This only forces the decision:
/// `0/false/no/off` disables it even with a goal, `1/true/yes/on` enables it even
/// without one.
pub const GUIDE_ENV: &str = "RPI_ORCHESTRATOR_GUIDE";

/// Project-relative path of `rpi-goal`'s state. An active goal there is what turns
/// the guidance on — a deliberate coupling to `/goal` mode.
const GOALS_FILE: &str = ".rpi/goals.json";

const GUIDE_HEADING: &str = "## Subagent orchestration";

const GUIDE_SECTION: &str = r#"## Subagent orchestration

For work that has several steps, or steps that can run in parallel, use the
`plan` and `dispatch` tools instead of doing everything inline:

1. `plan` with action=set: write the task graph as nodes, each with an `id`, a
   self-contained `task`, and its `deps`.
2. `dispatch`: run the current ready wave. Nodes whose dependencies are done run
   in parallel (up to `maxParallel`).
3. Read the results, then `plan` with action=update (add/remove/re-dep/reset
   nodes) and `dispatch` again until the goal is met.

A subagent starts fresh and cannot see this conversation, so each node's `task`
must contain everything it needs. Prefer this over hand-rolling the whole
workflow with `bash`."#;

/// Tri-state read of [`GUIDE_ENV`]: `Some(true)` force on, `Some(false)` force
/// off, `None` means "decide from the goal state".
fn guide_override(value: Option<&str>) -> Option<bool> {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "true" | "yes" | "on") => Some(true),
        Some("0" | "false" | "no" | "off") => Some(false),
        _ => None,
    }
}

/// Whether `rpi-goal` has an active goal in this project. Parsed leniently: any
/// read or parse problem simply means "no goal".
fn active_goal_from(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let Some(active) = value.get("active") else {
        return false;
    };
    !active.is_null() && active.get("status").and_then(Value::as_str) == Some("active")
}

fn active_goal_in_project() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    match std::fs::read_to_string(cwd.join(GOALS_FILE)) {
        Ok(text) => active_goal_from(&text),
        Err(_) => false,
    }
}

fn guide_enabled() -> bool {
    // Never inject into a subagent: it has no `plan`/`dispatch`, and telling it to
    // use them would just waste a turn.
    if std::env::var(crate::spawn::SUBAGENT_ENV).is_ok() {
        return false;
    }
    // `/goal` implies orchestration: an active goal turns the guidance on unless
    // the variable explicitly overrides the decision.
    guide_override(std::env::var(GUIDE_ENV).ok().as_deref()).unwrap_or_else(active_goal_in_project)
}

fn write_out(out: &mut rpi_plugin_sdk::callbacks::Output<'_>, value: Value) {
    if out.is_available() {
        out.write(value.to_string());
    }
}

rpi_plugin_sdk::string_handler! {
/// Append the orchestration guidance to the system prompt, only when opted in.
///
/// Replacement contract: the handler reads the base prompt from the event's
/// `systemPrompt` and writes `{"systemPrompt": "..."}` when it changed it, or
/// `{}` (no change) otherwise. A malformed envelope declines rather than
/// replacing the host's prompt with our section alone.
fn on_before_agent_start(event_json, out, _user_data) {
    if !guide_enabled() {
        write_out(out, serde_json::json!({}));
        return 0;
    }
    let parsed: Value = match serde_json::from_str(event_json) {
        Ok(value) => value,
        Err(_) => {
            write_out(out, serde_json::json!({}));
            return 0;
        }
    };
    let base = parsed
        .get("systemPrompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if base.contains(GUIDE_HEADING) {
        write_out(out, serde_json::json!({}));
        return 0;
    }
    let next = format!("{base}\n\n{GUIDE_SECTION}");
    write_out(out, serde_json::json!({"systemPrompt": next}));
    0
}
}

rpi_plugin_sdk::export_plugin!(|api| {
    // Capture the host action bridge first: `subagent`/`dispatch` read this
    // session's provider/model through it so a child inherits the parent binding
    // instead of quietly resolving the default provider.
    crate::host::install(api);
    if let Some(register_prompt) = api.register_before_agent_start {
        let _ = register_prompt(
            on_before_agent_start,
            rpi_plugin_sdk::callbacks::free_string,
            api.user_data,
        );
    }

    let Some(register) = api.register_tool else {
        return 1;
    };

    {
        let schema = Box::new(crate::kit::schema(
            "subagent",
            "Delegate one bounded task to a fresh rpi subprocess and return its final answer.",
            SUBAGENT_PARAMS,
        ));
        let rc = register(
            &*schema,
            subagent_tool::execute,
            subagent_tool::poll,
            subagent_tool::cancel,
            subagent_tool::destroy,
            crate::kit::plugin_free_string,
        );
        drop(schema);
        if rc != 0 {
            return 1;
        }
    }

    {
        let schema = Box::new(crate::kit::schema(
            "plan",
            "Author or edit the task DAG. action=set (replace with `nodes`), action=update (addNodes/removeIds/setDeps/reset), action=show. Edit only between dispatch waves; show is available during a wave. A node may declare `expectFile` (file that must exist after its child runs) and `verify` (shell command that must exit 0); `dispatch` enforces both. Completion is written only by `dispatch`, never by `plan`.",
            PLAN_PARAMS,
        ));
        let rc = register(
            &*schema,
            plan_tool::execute,
            plan_tool::poll,
            plan_tool::cancel,
            plan_tool::destroy,
            crate::kit::plugin_free_string,
        );
        drop(schema);
        if rc != 0 {
            return 1;
        }
    }

    {
        let schema = Box::new(crate::kit::schema(
            "dispatch",
            "Run one wave of the plan: every pending node whose dependencies are done, up to `maxParallel` subagents at once. Returns each node's result summary and the updated graph.",
            DISPATCH_PARAMS,
        ));
        let rc = register(
            &*schema,
            dispatch_tool::execute,
            dispatch_tool::poll,
            dispatch_tool::cancel,
            dispatch_tool::destroy,
            crate::kit::plugin_free_string,
        );
        drop(schema);
        if rc != 0 {
            return 1;
        }
    }
    0
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_env_override_is_tri_state() {
        for value in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert_eq!(guide_override(Some(value)), Some(true), "{value}");
        }
        for value in ["0", "false", "no", "off", " OFF "] {
            assert_eq!(guide_override(Some(value)), Some(false), "{value}");
        }
        assert_eq!(guide_override(None), None);
        assert_eq!(guide_override(Some("")), None);
        assert_eq!(guide_override(Some("whatever")), None);
    }

    #[test]
    fn active_goal_is_detected_from_the_project_state() {
        assert!(active_goal_from(
            r#"{"active":{"status":"active"},"archived":[]}"#
        ));
        assert!(!active_goal_from(r#"{"active":null,"archived":[]}"#));
        assert!(!active_goal_from(r#"{"active":{"status":"paused"}}"#));
        assert!(!active_goal_from(r#"{"active":{"status":"complete"}}"#));
        assert!(!active_goal_from("{}"));
        assert!(!active_goal_from("not json"));
    }

    /// The graph is process-global by design, so graph-mutating tests must not
    /// interleave. A poisoned lock is recovered (a failed test still releases it).
    static GRAPH_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn graph_test_guard() -> MutexGuard<'static, ()> {
        GRAPH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn params_for(session: Option<&str>) -> Value {
        match session {
            Some(id) => serde_json::json!({
                rpi_plugin_sdk::HOST_CONTEXT_KEY: { "cwd": "/tmp/p", "sessionId": id }
            }),
            None => serde_json::json!({}),
        }
    }

    #[test]
    fn cancellation_before_the_first_chunk_keeps_the_whole_plan_retryable() {
        let _serial = graph_test_guard();
        let mut params = params_for(Some("cancel-before-first-chunk"));
        params["action"] = serde_json::json!("set");
        params["maxParallel"] = serde_json::json!(1);
        params["nodes"] = serde_json::json!([
            {"id":"a", "task":"a"},
            {"id":"b", "task":"b"},
            {"id":"dependent", "task":"dependent", "deps":["a"]}
        ]);
        let control = crate::control::Control::default();
        let progress = std::sync::mpsc::channel().0;
        crate::plan::plan(&params, &control, &progress).unwrap();
        crate::dispatch::dispatch_with(
            &params,
            &control,
            &progress,
            |graph, ready, parallel, control, progress, parent| {
                // Cancel after dispatch marks the ready set running, but
                // before run_wave starts its first chunk. No child may run.
                control.cancel();
                crate::dispatch::run_wave(graph, ready, parallel, control, progress, parent)
            },
        )
        .unwrap();
        {
            let state = lock_graph(&params);
            for node in &state.graph().nodes {
                assert_eq!(
                    node.status,
                    graph::Status::Pending,
                    "unstarted node {} became {:?}",
                    node.id,
                    node.status
                );
            }
        }
        let control = crate::control::Control::default();
        let fixture = |_: &graph::Graph,
                       ready: &[String],
                       _: usize,
                       _: &crate::control::Control,
                       _: &crate::kit::Progress,
                       _: &crate::host::Selection| {
            ready
                .iter()
                .map(|id| (id.clone(), Ok("fixture success".into())))
                .collect()
        };
        // Retrying without resetting the graph completes the independent nodes,
        // then their dependent on the next wave. The fixture launches no child.
        crate::dispatch::dispatch_with(&params, &control, &progress, fixture).unwrap();
        assert_eq!(lock_graph(&params).graph().ready(), vec!["dependent"]);
        crate::dispatch::dispatch_with(&params, &control, &progress, fixture).unwrap();
        assert!(lock_graph(&params)
            .graph()
            .nodes
            .iter()
            .all(|node| node.status == graph::Status::Done));
    }

    #[test]
    fn verification_keeps_plan_readable_and_does_not_commit_into_a_new_session() {
        use std::time::{Duration, Instant};
        let _serial = graph_test_guard();
        let marker_name = format!(
            "rpi-verify-review-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let marker = std::env::current_dir().unwrap().join(&marker_name);
        #[cfg(windows)]
        let verify = format!("echo started>{marker_name} & ping -n 31 127.0.0.1 >nul");
        #[cfg(unix)]
        let verify = format!(
            "printf started > '{}'; sleep 30",
            marker.to_string_lossy().replace('\'', "'\\''")
        );
        let mut params = params_for(Some("verification-a"));
        params["action"] = serde_json::json!("set");
        params["nodes"] = serde_json::json!([{"id":"x", "task":"original", "verify":verify}]);
        let control = crate::control::Control::default();
        crate::plan::plan(&params, &control, &std::sync::mpsc::channel().0).unwrap();
        let wave_params = params.clone();
        let wave_control = control.clone();
        let wave = std::thread::spawn(move || {
            crate::dispatch::dispatch_with(
                &wave_params,
                &wave_control,
                &std::sync::mpsc::channel().0,
                |_, ready, _, _, _, _| {
                    ready
                        .iter()
                        .map(|id| (id.clone(), Ok("fixture result".into())))
                        .collect()
                },
            )
        });
        let setup_deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < setup_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let verification_started = marker.exists();
        let (tx, rx) = std::sync::mpsc::channel();
        let show_params = params_for(Some("verification-a"));
        let show = std::thread::spawn(move || {
            let _ = tx.send(crate::plan::plan(
                &show_params,
                &crate::control::Control::default(),
                &std::sync::mpsc::channel().0,
            ));
        });
        let readable = rx.recv_timeout(Duration::from_millis(500));
        let replacement = if matches!(&readable, Ok(Ok(_))) {
            let editing = crate::plan::plan(&params, &control, &std::sync::mpsc::channel().0);
            let mut replacement = params_for(Some("verification-b"));
            replacement["action"] = serde_json::json!("set");
            replacement["nodes"] = serde_json::json!([{"id":"x", "task":"replacement"}]);
            let result = crate::plan::plan(&replacement, &control, &std::sync::mpsc::channel().0);
            Some((editing, result, replacement))
        } else {
            None
        };
        // Clean up both workers before any assertion, including on old code
        // where the show worker is blocked behind the verification command.
        control.cancel();
        let wave_result = wave.join().unwrap();
        show.join().unwrap();
        let _ = std::fs::remove_file(marker);
        assert!(
            verification_started,
            "verification fixture did not start: {wave_result:?}"
        );
        assert!(
            matches!(&readable, Ok(Ok(_))),
            "plan show blocked during verification: {readable:?}"
        );
        let (editing, result, replacement) = replacement.unwrap();
        assert!(editing.unwrap_err().contains("dispatch is running"));
        result.unwrap();
        assert!(wave_result.unwrap_err().contains("stale results discarded"));
        let state = lock_graph(&replacement);
        assert_eq!(state.graph().get("x").unwrap().task, "replacement");
        assert_eq!(
            state.graph().get("x").unwrap().status,
            graph::Status::Pending
        );
    }

    #[test]
    fn running_wave_rejects_plan_edits_and_overlapping_dispatch() {
        let _serial = graph_test_guard();
        let params = params_for(Some("busy-wave"));
        let control = crate::control::Control::default();
        let progress = std::sync::mpsc::channel().0;
        let mut set = params.clone();
        set["action"] = serde_json::json!("set");
        set["nodes"] = serde_json::json!([{"id":"x", "task":"original"}]);
        crate::plan::plan(&set, &control, &progress).unwrap();
        let generation = {
            let mut guard = lock_graph(&params);
            guard.graph_mut().get_mut("x").unwrap().status = graph::Status::Running;
            guard.generation()
        };

        let mut update = params.clone();
        update["action"] = serde_json::json!("update");
        update["reset"] = serde_json::json!(["x"]);
        for edit in [&set, &update] {
            assert!(crate::plan::plan(edit, &control, &progress)
                .unwrap_err()
                .contains("dispatch is running"));
        }
        assert!(crate::plan::plan(&params, &control, &progress)
            .unwrap()
            .contains("original"));
        assert!(crate::dispatch::dispatch(&params, &control, &progress)
            .unwrap_err()
            .contains("already running"));
        {
            let mut guard = lock_graph_for_wave(&params, generation).unwrap();
            assert_eq!(
                guard.graph().get("x").unwrap().status,
                graph::Status::Running
            );
            guard.graph_mut().get_mut("x").unwrap().status = graph::Status::Done;
        }
        crate::plan::plan(&update, &control, &progress).unwrap();
        assert_eq!(
            lock_graph(&params).graph().get("x").unwrap().status,
            graph::Status::Pending
        );
        assert!(lock_graph_for_wave(&params, generation).is_err());
    }

    #[test]
    fn stale_wave_cannot_reset_new_session_or_touch_reused_node_ids() {
        let _serial = graph_test_guard();
        let a = params_for(Some("wave-a"));
        let b = params_for(Some("wave-b"));
        let old_generation = lock_graph(&a).generation();
        let control = crate::control::Control::default();
        let progress = std::sync::mpsc::channel().0;
        let mut replacement = b.clone();
        replacement["action"] = serde_json::json!("set");
        replacement["nodes"] = serde_json::json!([{"id":"x", "task":"replacement"}]);
        crate::plan::plan(&replacement, &control, &progress).unwrap();
        assert!(lock_graph_for_wave(&a, old_generation).is_err());
        {
            // Inspect directly: using lock_graph(&b) would hide an accidental
            // reset by a stale completion by switching the session back again.
            let guard = graph_state().lock().unwrap();
            assert_eq!(guard.session, session_of(&b));
            assert_eq!(guard.graph().get("x").unwrap().task, "replacement");
            assert_eq!(
                guard.graph().get("x").unwrap().status,
                graph::Status::Pending
            );
        }
        // Returning to A still must not accept an old A wave's results.
        replacement[rpi_plugin_sdk::HOST_CONTEXT_KEY] = a[rpi_plugin_sdk::HOST_CONTEXT_KEY].clone();
        crate::plan::plan(&replacement, &control, &progress).unwrap();
        assert!(lock_graph_for_wave(&a, old_generation).is_err());
        assert_eq!(
            lock_graph(&a).graph().get("x").unwrap().status,
            graph::Status::Pending
        );
    }

    /// Regression: the graph used to be one process-global value, so session B
    /// saw (and could dispatch) session A's plan. Each session now owns its own
    /// graph, and a switch resets rather than replays.
    #[test]
    fn graphs_are_isolated_per_session() {
        let _serial = graph_test_guard();
        let a = params_for(Some("session-a"));
        let b = params_for(Some("session-b"));

        let result = crate::plan::plan(
            &serde_json::json!({
                "action": "set",
                "nodes": [{"id": "only-a", "task": "a-task"}],
                rpi_plugin_sdk::HOST_CONTEXT_KEY: { "sessionId": "session-a" }
            }),
            &crate::control::Control::default(),
            &std::sync::mpsc::channel().0,
        )
        .unwrap();
        assert!(result.contains("only-a"));

        {
            let guard = lock_graph(&a);
            assert_eq!(guard.graph().nodes.len(), 1);
            assert!(guard.graph().get("only-a").is_some());
        }
        // A different session must not inherit that plan.
        {
            let guard = lock_graph(&b);
            assert!(
                guard.graph().nodes.is_empty(),
                "session B saw session A's plan"
            );
        }
    }

    /// Switching back to the first session starts clean rather than replaying:
    /// resuming an old plan is the human's decision.
    #[test]
    fn returning_to_a_session_does_not_resurrect_the_old_plan() {
        let _serial = graph_test_guard();
        let a = params_for(Some("session-a"));
        let b = params_for(Some("session-b"));
        for params in [&a, &b] {
            let mut guard = lock_graph(params);
            *guard.graph_mut() = graph::Graph::default();
            guard.graph_mut().nodes.push(
                graph::Node::from_value(&serde_json::json!({"id": "x", "task": "t"})).unwrap(),
            );
        }
        // Touch B, then come back to A: A's graph is reset, not restored.
        drop(lock_graph(&b));
        let guard = lock_graph(&a);
        assert!(
            guard.graph().nodes.is_empty(),
            "a stale plan survived a session round-trip"
        );
    }

    /// A host without `__rpi` (or an ephemeral session with no id) still works:
    /// it shares the single `None` bucket instead of refusing to serve.
    #[test]
    fn missing_session_id_falls_back_to_one_shared_graph() {
        let _serial = graph_test_guard();
        let none = params_for(None);
        assert!(session_of(&none).is_none());
        {
            let mut guard = lock_graph(&none);
            *guard.graph_mut() = graph::Graph::from_nodes_value(&serde_json::json!([
                {"id": "fallback", "task": "t"}
            ]))
            .unwrap();
        }
        let guard = lock_graph(&params_for(None));
        assert!(guard.graph().get("fallback").is_some());
    }
}
