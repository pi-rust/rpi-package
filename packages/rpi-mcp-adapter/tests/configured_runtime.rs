//! Typed, in-memory MCP authority exercised against an actual Rust subprocess.
#![forbid(unsafe_code)]
use rpi_mcp_core::{
    ConfiguredStdio, Control, CoreRequestContext, DiscoveryLimits, McpRuntime, StdioServer,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(5);
struct Fixture {
    dir: PathBuf,
    trace: PathBuf,
    target: ConfiguredStdio,
}
impl Fixture {
    fn new(modes: &[&str], timeout: u64) -> Self {
        let dir = std::env::temp_dir().join(format!("regression-worker-typed-{}", uuid_suffix()));
        fs::create_dir(&dir).unwrap();
        let trace = dir.join("trace.jsonl");
        let context = CoreRequestContext {
            scope: "typed-fixture".into(),
            cwd: dir.clone(),
        };
        let mut args: Vec<String> = modes.iter().map(|s| (*s).into()).collect();
        args.push(format!("--ownership-log={}", trace.display()));
        // Empty argv and environment values are legal subprocess inputs.
        args.push(String::new());
        let target = ConfiguredStdio::new(
            &context,
            "server / 中文",
            StdioServer {
                command: env!("CARGO_BIN_EXE_fake_mcp_server").into(),
                args,
                env: BTreeMap::from([("RPI_EMPTY_VALUE".into(), String::new())]),
                cwd: None,
                enabled: true,
                timeout,
                exposure: Default::default(),
            },
        )
        .unwrap();
        Self { dir, trace, target }
    }
    fn records(&self) -> Vec<Value> {
        fs::read_to_string(&self.trace)
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
    fn requests(&self, method: &str) -> usize {
        self.records()
            .iter()
            .filter(|r| r["event"] == "request" && r["details"]["method"] == method)
            .count()
    }
    fn pid(&self) -> u32 {
        self.records()
            .into_iter()
            .find(|r| r["event"] == "start")
            .unwrap()["pid"]
            .as_u64()
            .unwrap() as u32
    }
    fn discover(&self, rt: &McpRuntime) {
        let found = rt
            .discover_stdio(&self.target, limits(), &Control::default())
            .unwrap();
        assert_eq!(found.name, "server / 中文");
        assert!(!found.tools.is_empty());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !thread::panicking() {
            fs::remove_dir_all(&self.dir).unwrap();
        } else {
            eprintln!("fixture artifacts: {}", self.dir.display());
        }
    }
}
fn uuid_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}
fn limits() -> DiscoveryLimits {
    DiscoveryLimits {
        max_tools: 128,
        max_catalog_bytes: 256 * 1024,
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + WINDOW;
    while !predicate() {
        assert!(Instant::now() < end, "fixture observation timed out");
        thread::sleep(Duration::from_millis(10));
    }
}
#[cfg(windows)]
fn running(pid: u32) -> bool {
    rpi_plugin_sdk::process::process_is_running(pid).unwrap()
}
#[cfg(unix)]
fn running(pid: u32) -> bool {
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        return !stat.rsplit_once(") ").unwrap().1.starts_with('Z');
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    if out.status.success() {
        return !String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .starts_with('Z');
    }
    assert_eq!(out.status.code(), Some(1));
    false
}

#[test]
fn typed_calls_preserve_raw_results_and_never_requery_discovery() {
    let f = Fixture::new(&["--paged", "--typed-raw"], 5);
    let rt = McpRuntime::new();
    assert!(rt
        .call_stdio(&f.target, "echo", json!({}), &Control::default())
        .is_err());
    assert!(
        f.records().is_empty(),
        "call before discovery spawned a server"
    );
    f.discover(&rt);
    assert!(running(f.pid()));
    let lists = f.requests("tools/list");
    assert_eq!(lists, 2);
    let args = json!({"value":"caller", "__rpi":{"sessionId":"not-authority"}});
    for target in [f.target.clone(), f.target.clone()] {
        let raw = rt
            .call_stdio(&target, "typed_raw", args.clone(), &Control::default())
            .unwrap();
        assert_eq!(
            raw,
            json!({"isError":true,"content":[{"type":"text","text":"raw error","annotations":{"priority":0.5}}],"structuredContent":{"arguments":args},"opaqueMetadata":{"retained":true}})
        );
    }
    assert_eq!(f.requests("tools/list"), lists);
    assert_eq!(f.requests("initialize"), 1);
    assert_eq!(f.requests("tools/call"), 2);
    let pid = f.pid();
    drop(rt);
    wait(|| !running(pid));
}

#[test]
fn real_discovery_rejects_count_bytes_unknown_metadata_duplicates_and_invalid_schema() {
    for (modes, bounds, expected) in [
        (
            vec!["--paged"],
            DiscoveryLimits {
                max_tools: 1,
                max_catalog_bytes: 65536,
            },
            "count",
        ),
        (
            vec!["--catalog-metadata"],
            DiscoveryLimits {
                max_tools: 128,
                max_catalog_bytes: 1024,
            },
            "byte",
        ),
        (vec!["--duplicate-tools"], limits(), "duplicate"),
        (vec!["--invalid-schema"], limits(), "object"),
    ] {
        let f = Fixture::new(&modes, 5);
        let rt = McpRuntime::new();
        let error = rt
            .discover_stdio(&f.target, bounds, &Control::default())
            .unwrap_err();
        assert!(error.contains(expected), "{error}");
        let before = f.records();
        assert!(rt
            .call_stdio(&f.target, "echo", json!({}), &Control::default())
            .is_err());
        assert_eq!(f.records(), before, "failed discovery still admitted calls");
        let pid = f.pid();
        drop(rt);
        wait(|| !running(pid));
    }
}

#[test]
fn cancelled_blocked_typed_call_releases_worker_and_requires_fresh_discovery() {
    let f = Fixture::new(&["--hang-call"], 300);
    let rt = Arc::new(McpRuntime::new());
    f.discover(&rt);
    let pid = f.pid();
    assert!(running(pid));
    let control = Control::default();
    let call_control = control.clone();
    let owner = rt.clone();
    let target = f.target.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        tx.send(owner.call_stdio(&target, "echo", json!({}), &call_control))
            .unwrap();
    });
    wait(|| f.requests("tools/call") == 1);
    control.cancel();
    assert!(rx
        .recv_timeout(WINDOW)
        .expect("cancel did not release actual call")
        .is_err());
    worker.join().unwrap();
    wait(|| !running(pid));
    let before = f.records();
    assert!(rt
        .call_stdio(&f.target, "echo", json!({}), &Control::default())
        .is_err());
    assert_eq!(before, f.records(), "retired call reconnected or replayed");
    assert_eq!(Arc::strong_count(&control.0), 1);
    drop(rt);
}

#[test]
fn mixed_method_and_response_followed_by_valid_reply_retires_the_connection() {
    for mode in ["--mixed-list", "--mixed-call"] {
        let f = Fixture::new(&[mode], 5);
        let rt = McpRuntime::new();
        let error = if mode == "--mixed-list" {
            rt.discover_stdio(&f.target, limits(), &Control::default())
                .unwrap_err()
        } else {
            f.discover(&rt);
            assert!(running(f.pid()));
            rt.call_stdio(&f.target, "echo", json!({}), &Control::default())
                .unwrap_err()
        };
        let expected = if mode == "--mixed-list" {
            "MCP stdio tools/list failed"
        } else {
            "MCP stdio tools/call failed"
        };
        assert_eq!(
            error, expected,
            "typed errors must not expose transport data"
        );
        wait(|| !running(f.pid()));
        let before = f.records();
        assert!(rt
            .call_stdio(&f.target, "echo", json!({}), &Control::default())
            .is_err());
        assert_eq!(
            f.records(),
            before,
            "mixed frame admitted a reconnect/replay"
        );
        assert_eq!(f.requests("initialize"), 1);
        drop(rt);
    }
}

#[test]
fn timed_out_typed_call_does_not_cancel_caller_or_reconnect() {
    let f = Fixture::new(&["--hang-call"], 1);
    let rt = McpRuntime::new();
    f.discover(&rt);
    let pid = f.pid();
    let control = Control::default();
    let start = Instant::now();
    assert!(rt
        .call_stdio(&f.target, "echo", json!({}), &control)
        .is_err());
    assert!(start.elapsed() < WINDOW);
    assert!(
        control.check().is_ok(),
        "own deadline poisoned caller cancellation"
    );
    assert_eq!(f.requests("tools/call"), 1);
    wait(|| !running(pid));
    let before = f.records();
    assert!(rt
        .call_stdio(&f.target, "echo", json!({}), &Control::default())
        .is_err());
    assert_eq!(f.records(), before);
    drop(rt);
}
