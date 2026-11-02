//! Caller-owned core lifecycle contracts, exercised with real stdio children.
//! No SDK session globals, network servers, or production configuration are used.
#![forbid(unsafe_code)]

use rpi_mcp_core::{Control, CoreRequestContext, McpRuntime, ResourceUpdateBatch};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const WINDOW: Duration = Duration::from_secs(5);
const URI: &str = "mem://ownership";

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mcp-ownership-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        // Keep dynamic repro config and process traces on assertion failure.
        if thread::panicking() {
            eprintln!("ownership failure artifacts: {}", self.0.display());
        } else {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

struct Fixture {
    dir: TempDir,
    config: PathBuf,
    trace: PathBuf,
}
impl Fixture {
    fn new(modes: &[&str], timeout: u64) -> Self {
        let dir = TempDir::new();
        let trace = dir.0.join("process.jsonl");
        let config = dir.0.join("mcp.json");
        let mut args: Vec<String> = modes.iter().map(|m| (*m).into()).collect();
        args.push("--process-info".into());
        args.push(format!("--ownership-log={}", trace.display()));
        fs::write(
            &config,
            json!({"mcpServers":{"fake":{
                "command":env!("CARGO_BIN_EXE_fake_mcp_server"),
                "args":args,"timeout":timeout,
                "env":{"RPI_OWNERSHIP_PROFILE":"explicit-fixture-profile"}
            }}})
            .to_string(),
        )
        .unwrap();
        Self { dir, config, trace }
    }
    fn context(&self, scope: &str) -> CoreRequestContext {
        CoreRequestContext {
            scope: scope.into(),
            cwd: self.dir.0.clone(),
        }
    }
    fn params(&self) -> Value {
        json!({"configPath":self.config,"server":"fake"})
    }
    fn call_params(&self, value: &str) -> Value {
        let mut p = self.params();
        p["tool"] = json!("echo");
        p["arguments"] = json!({"value":value});
        p
    }
    fn call(&self, rt: &McpRuntime, ctx: &CoreRequestContext, value: &str) -> Value {
        rt.call(ctx, &self.call_params(value), &Control::default())
            .unwrap()
    }
    fn pid(&self, rt: &McpRuntime, ctx: &CoreRequestContext) -> u32 {
        pid(&self.call(rt, ctx, "identity"))
    }
    fn subscribe(&self, rt: &McpRuntime, ctx: &CoreRequestContext) {
        let mut p = self.params();
        p["action"] = json!("subscribe");
        p["uri"] = json!(URI);
        assert_eq!(
            rt.resources(ctx, &p, &Control::default()).unwrap()["subscribed"],
            true
        );
    }
    fn close(&self, rt: &McpRuntime, ctx: &CoreRequestContext) {
        let mut p = self.params();
        p["action"] = json!("close");
        assert_eq!(
            rt.list(ctx, &p, &Control::default()).unwrap()["closed"],
            true
        );
    }
    fn records(&self) -> Vec<Value> {
        fs::read_to_string(&self.trace)
            .unwrap_or_default()
            .lines()
            // A concurrent append can expose an incomplete final record.
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn wait_record(&self, event: &str, method: Option<&str>) -> Value {
        let mut found = None;
        wait_until(
            || {
                found = self.records().into_iter().find(|r| {
                    r["event"] == event && method.map_or(true, |m| r["details"]["method"] == m)
                });
                found.is_some()
            },
            &format!("{event}/{method:?} in {}", self.trace.display()),
        );
        found.unwrap()
    }
    fn descendant(&self) -> u32 {
        let child = self.wait_record("descendant", None)["details"]["pid"]
            .as_u64()
            .unwrap() as u32;
        // Also witness that the spawned leaf entered main, not just spawn().
        wait_until(
            || {
                self.records().iter().any(|r| {
                    r["pid"] == child && r["event"] == "start" && r["details"]["leaf"] == true
                })
            },
            "leaf startup",
        );
        assert_running(child);
        child
    }
}

fn pid(result: &Value) -> u32 {
    result["structuredContent"]["pid"].as_u64().unwrap() as u32
}
fn wait_until(mut predicate: impl FnMut() -> bool, description: &str) {
    let deadline = Instant::now() + WINDOW;
    loop {
        if predicate() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
#[cfg(windows)]
fn is_running(pid: u32) -> bool {
    rpi_plugin_sdk::process::process_is_running(pid).unwrap()
}
#[cfg(unix)]
fn is_running(pid: u32) -> bool {
    // An exited, unreaped child still answers kill -0. Linux exposes its state
    // in /proc; macOS needs ps. Do not conflate zombie lifetime with execution.
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        if let Some((_, state)) = stat.rsplit_once(") ") {
            return !state.starts_with('Z');
        }
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("process-state probe must execute");
    let state = std::str::from_utf8(&output.stdout).unwrap().trim();
    if output.status.success() {
        assert!(!state.is_empty(), "ps succeeded without a process state");
        return !state.starts_with('Z');
    }
    assert!(
        output.status.code() == Some(1) && state.is_empty() && output.stderr.is_empty(),
        "process-state probe failed: {output:?}"
    );
    false // ps reports no matching PID, not an observation/permission error.
}

#[cfg(unix)]
#[test]
fn exited_unreaped_child_is_not_observed_as_running() {
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Reap(
        std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap(),
    );
    let pid = child.0.id();
    assert_running(pid);
    child.0.kill().unwrap();
    // Intentionally observe exit before wait(), matching the dead MCP leader.
    // Reap still owns cleanup if an assertion panics; no fixture is left behind.
    assert_exited(pid);
    child.0.wait().unwrap();
}
fn assert_running(pid: u32) {
    assert!(
        is_running(pid),
        "fixture process {pid} was never observed alive"
    );
}
fn assert_exited(pid: u32) {
    wait_until(|| !is_running(pid), &format!("actual process {pid} exit"));
}
fn bounded_drop(rt: Arc<McpRuntime>) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        drop(rt);
        let _ = tx.send(());
    });
    rx.recv_timeout(WINDOW)
        .expect("last-owner Drop failed to finish within five seconds");
}
fn take_exact(rt: &McpRuntime, scope: &str, value: &str) -> ResourceUpdateBatch {
    let expected = format!("{URI}\n{value}");
    let mut ready = None;
    wait_until(
        || {
            let batch = rt.take_resource_update_batch(None);
            let updates: Vec<_> = batch.updates().collect();
            if updates.len() == 1
                && updates[0].scope == scope
                && updates[0].uri == URI
                && updates[0].text == expected
            {
                ready = Some(batch);
                true
            } else {
                batch.retry();
                false
            }
        },
        &format!("exact update {scope}/{expected:?}"),
    );
    ready.unwrap()
}
fn queue(
    f: &Fixture,
    rt: &McpRuntime,
    ctx: &CoreRequestContext,
    value: &str,
) -> ResourceUpdateBatch {
    f.subscribe(rt, ctx);
    f.call(rt, ctx, value);
    take_exact(rt, &ctx.scope, value)
}

#[test]
fn independent_runtimes_same_scope_and_config_have_distinct_live_children() {
    let f = Fixture::new(&[], 5);
    let ctx = f.context("same-scope");
    let a = McpRuntime::new();
    let b = McpRuntime::new();
    let a_pid = f.pid(&a, &ctx);
    // Resolve a syntactically different path to the same canonical file.
    let mut p = f.call_params("identity");
    p["configPath"] = json!(f.dir.0.join(".").join("mcp.json"));
    let b_pid = pid(&b.call(&ctx, &p, &Control::default()).unwrap());
    assert_ne!(a_pid, b_pid, "independent owners reused a process");
    assert_running(a_pid);
    assert_running(b_pid);
    assert_eq!(a_pid, f.pid(&a, &ctx));
    assert_eq!(b_pid, f.pid(&b, &ctx));
    f.close(&a, &ctx);
    assert_exited(a_pid);
    assert_running(b_pid);
    assert_eq!(b_pid, f.pid(&b, &ctx));
    drop(a);
    assert_eq!(b_pid, f.pid(&b, &ctx));
    drop(b);
    assert_exited(b_pid);
}

#[test]
fn closing_and_dropping_a_preserves_b_queue_and_subscription() {
    let f = Fixture::new(&["--stateful-resources", "--delivery-values"], 5);
    let ctx = f.context("same-scope");
    let a = McpRuntime::new();
    let b = McpRuntime::new();
    let a_pid = f.pid(&a, &ctx);
    let b_pid = f.pid(&b, &ctx);
    queue(&f, &a, &ctx, "only-A").retry();
    queue(&f, &b, &ctx, "only-B").retry();
    f.close(&a, &ctx);
    assert_exited(a_pid);
    assert!(a.take_resource_updates(None).is_empty());
    // Exact values, not a shared default string, establish queue isolation.
    let b_batch = take_exact(&b, &ctx.scope, "only-B");
    b_batch.retry();
    let reopened_a = f.pid(&a, &ctx);
    assert_running(reopened_a);
    drop(a); // Drop an owner with a live connection, not only an empty registry.
    assert_exited(reopened_a);
    drop(take_exact(&b, &ctx.scope, "only-B"));
    f.call(&b, &ctx, "B-after-A-drop");
    drop(take_exact(&b, &ctx.scope, "B-after-A-drop"));
    assert_eq!(b_pid, f.pid(&b, &ctx));
    drop(b);
    assert_exited(b_pid);
}

#[test]
fn shutdown_drains_only_its_owner_and_can_reopen() {
    let f = Fixture::new(&["--stateful-resources", "--delivery-values"], 5);
    let ctx = f.context("same-scope");
    let a = McpRuntime::new();
    let b = McpRuntime::new();
    let a_pid = f.pid(&a, &ctx);
    let b_pid = f.pid(&b, &ctx);
    let old_batch = queue(&f, &a, &ctx, "retired-A");
    f.call(&a, &ctx, "queued-A");
    take_exact(&a, &ctx.scope, "queued-A").retry();
    queue(&f, &b, &ctx, "surviving-B").retry();
    a.shutdown_connections();
    a.shutdown_connections(); // idempotent
    assert_exited(a_pid);
    old_batch.retry();
    assert!(a.take_resource_updates(None).is_empty());
    drop(take_exact(&b, &ctx.scope, "surviving-B"));
    assert_eq!(b_pid, f.pid(&b, &ctx));
    let reopened = f.pid(&a, &ctx);
    assert_ne!(a_pid, reopened);
    assert!(a.take_resource_updates(None).is_empty());
    drop(queue(&f, &a, &ctx, "fresh-A"));
    drop(a);
    assert_exited(reopened);
    assert_running(b_pid);
}

#[test]
fn detached_batch_does_not_keep_owner_or_child_alive_and_retry_cannot_resurrect() {
    let f = Fixture::new(&["--stateful-resources", "--delivery-values"], 5);
    let ctx = f.context("detached");
    let rt = Arc::new(McpRuntime::new());
    let child = f.pid(&rt, &ctx);
    let batch = queue(&f, &rt, &ctx, "detached-payload");
    let weak = Arc::downgrade(&rt);
    bounded_drop(rt);
    assert!(weak.upgrade().is_none());
    assert_exited(child);
    let before = f.records();
    batch.retry(); // expired weak state: safe no-op, not a reconnect
    assert_eq!(before, f.records());
    let next = McpRuntime::new();
    assert!(next.take_resource_updates(None).is_empty());
    assert_ne!(child, f.pid(&next, &ctx));
    assert!(next.take_resource_updates(None).is_empty());
}

#[test]
fn scopes_in_one_runtime_isolate_children_close_and_resource_queues() {
    let f = Fixture::new(&["--stateful-resources", "--delivery-values"], 5);
    let a = f.context("scope-A");
    let b = f.context("scope-B");
    let rt = McpRuntime::new();
    let a_pid = f.pid(&rt, &a);
    let b_pid = f.pid(&rt, &b);
    assert_ne!(a_pid, b_pid);
    queue(&f, &rt, &a, "scope-A-only").retry();
    f.subscribe(&rt, &b);
    f.call(&rt, &b, "scope-B-only");
    let mut updates = Vec::new();
    wait_until(
        || {
            updates.extend(rt.take_resource_updates(None));
            updates.len() >= 2
        },
        "both scoped updates",
    );
    updates.sort_by(|x, y| x.scope.cmp(&y.scope));
    assert_eq!(updates.len(), 2);
    assert_eq!(
        (&*updates[0].scope, &*updates[0].text),
        ("scope-A", "mem://ownership\nscope-A-only")
    );
    assert_eq!(
        (&*updates[1].scope, &*updates[1].text),
        ("scope-B", "mem://ownership\nscope-B-only")
    );
    f.call(&rt, &b, "B-queued-during-A-close");
    take_exact(&rt, &b.scope, "B-queued-during-A-close").retry();
    f.close(&rt, &a);
    assert_exited(a_pid);
    drop(take_exact(&rt, &b.scope, "B-queued-during-A-close"));
    f.call(&rt, &b, "B-still-subscribed");
    drop(take_exact(&rt, &b.scope, "B-still-subscribed"));
    assert_eq!(b_pid, f.pid(&rt, &b));
    drop(rt);
    assert_exited(b_pid);
}

#[test]
fn explicit_context_controls_relative_config_child_cwd_env_and_ignores_host_fields() {
    let f = Fixture::new(&["--stateful-resources", "--delivery-values"], 5);
    let ctx = f.context("explicit-authority");
    let rt = McpRuntime::new();
    let mut params = f.call_params("from-explicit-context");
    params["configPath"] = json!("mcp.json");
    params["cwd"] = json!("/not-the-caller");
    params["scope"] = json!("forged-top-level");
    params["env"] = json!({"RPI_OWNERSHIP_PROFILE":"forged"});
    params["__rpi"] =
        json!({"sessionId":"forged-session", "cwd":"/does-not-exist", "scope":"forged-scope"});
    let result = rt.call(&ctx, &params, &Control::default()).unwrap();
    let child = pid(&result);
    assert_running(child);
    assert_eq!(
        fs::canonicalize(result["structuredContent"]["cwd"].as_str().unwrap()).unwrap(),
        fs::canonicalize(&ctx.cwd).unwrap()
    );
    assert_eq!(
        result["structuredContent"]["profile"],
        "explicit-fixture-profile"
    );
    assert_eq!(child, f.pid(&rt, &ctx));
    f.subscribe(&rt, &ctx);
    rt.call(&ctx, &params, &Control::default()).unwrap();
    let batch = take_exact(&rt, &ctx.scope, "from-explicit-context");
    let key: Value = serde_json::from_str(&batch.updates().next().unwrap().key).unwrap();
    assert_eq!(key[0], ctx.scope);
    assert_eq!(
        key[1],
        fs::canonicalize(&f.config)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    drop(batch);
    // Relative configured cwd is anchored to the config, not the caller cwd.
    let work = f.dir.0.join("child-work");
    fs::create_dir(&work).unwrap();
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&f.config).unwrap()).unwrap();
    config["mcpServers"]["fake"]["cwd"] = json!("child-work");
    fs::write(&f.config, config.to_string()).unwrap();
    let caller = f.dir.0.join("other-caller");
    fs::create_dir(&caller).unwrap();
    let other = CoreRequestContext {
        scope: "configured-cwd".into(),
        cwd: caller,
    };
    let result = f.call(&rt, &other, "configured");
    assert_eq!(
        fs::canonicalize(Path::new(
            result["structuredContent"]["cwd"].as_str().unwrap()
        ))
        .unwrap(),
        fs::canonicalize(work).unwrap()
    );
    assert_eq!(
        result["structuredContent"]["profile"],
        "explicit-fixture-profile"
    );
    assert_eq!(CoreRequestContext::default().scope, "library");
}

#[test]
fn last_owner_drop_cancels_trace_confirmed_background_read_and_kills_tree() {
    let f = Fixture::new(&["--healthy-child", "--notify-early", "--hang-read"], 300);
    let ctx = f.context("blocked-dispatcher");
    let rt = Arc::new(McpRuntime::new());
    let leader = f.pid(&rt, &ctx);
    let leaf = f.descendant();
    f.subscribe(&rt, &ctx);
    // subscribe has returned: no foreground method borrow remains. The
    // dispatcher is blocked in resources/read, witnessed by the server trace.
    f.wait_record("request", Some("resources/read"));
    let last = Arc::clone(&rt);
    drop(rt);
    bounded_drop(last);
    assert_exited(leader);
    assert_exited(leaf);
    // Process exit + bounded Drop is observed; this test does not introspect
    // every worker's JoinHandle. The implementation owns those joins.
}

#[test]
fn owner_drop_kills_leaf_holding_pipes_after_leader_has_exited() {
    let f = Fixture::new(&["--healthy-child", "--exit-leader"], 5);
    let ctx = f.context("leader-already-exited");
    let rt = Arc::new(McpRuntime::new());
    let leader = f.pid(&rt, &ctx);
    assert_running(leader);
    let leaf = f.descendant();
    assert_eq!(leader, pid(&f.call(&rt, &ctx, "exit-leader")));
    f.wait_record("leader-exit", None);
    assert_exited(leader);
    assert_running(leaf);
    bounded_drop(rt);
    assert_exited(leaf);
}

#[test]
fn cancelled_resource_request_releases_control_and_real_process_tree() {
    let f = Fixture::new(&["--healthy-child", "--hang-read"], 300);
    let ctx = f.context("cancel-resource");
    let rt = Arc::new(McpRuntime::new());
    let leader = f.pid(&rt, &ctx);
    let leaf = f.descendant();
    let mut p = f.params();
    p["action"] = json!("read");
    p["uri"] = json!(URI);
    let control = Control::default();
    let worker_control = control.clone();
    let worker_rt = Arc::clone(&rt);
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = worker_rt.resources(&ctx, &p, &worker_control);
        tx.send(result).unwrap();
    });
    f.wait_record("request", Some("resources/read"));
    control.cancel();
    let error = rx
        .recv_timeout(WINDOW)
        .expect("cancel did not release request")
        .unwrap_err();
    assert!(error.contains("cancelled"), "{error}");
    worker.join().unwrap();
    assert_eq!(
        Arc::strong_count(&control.0),
        1,
        "request retained caller control after completion"
    );
    assert_exited(leader);
    assert_exited(leaf);
    bounded_drop(rt);
}

#[test]
fn timed_out_resource_request_releases_control_and_real_process_tree() {
    let f = Fixture::new(&["--healthy-child", "--hang-read"], 1);
    let ctx = f.context("timeout-resource");
    let rt = Arc::new(McpRuntime::new());
    let leader = f.pid(&rt, &ctx);
    let leaf = f.descendant();
    let mut p = f.params();
    p["action"] = json!("read");
    p["uri"] = json!(URI);
    let control = Control::default();
    let start = Instant::now();
    let error = rt.resources(&ctx, &p, &control).unwrap_err();
    assert!(
        error.contains("resources/read timed out after 1 seconds"),
        "{error}"
    );
    assert!(start.elapsed() < WINDOW, "timeout cleanup was unbounded");
    f.wait_record("request", Some("resources/read"));
    assert_eq!(Arc::strong_count(&control.0), 1);
    assert_exited(leader);
    assert_exited(leaf);
    bounded_drop(rt);
}
