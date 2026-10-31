//! Real MCP process regressions for the session-owned bridge.
//! Run with `cargo test -p rpi-extcore --features test-support --test mcp_stdio_bridge`.
//! The native peer is in memory; MCP is an actual OS-shell child (no Python,
//! cross-package binary lookup, test-harness stdout, or nested Cargo invocation).
#![cfg(feature = "test-support")]
#![forbid(unsafe_code)]
use rpi_extcore::{
    external_tools::StdioToolServer,
    host::{HostSession, TestEvents},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream};
const WINDOW: Duration = Duration::from_secs(8);

struct Fixture {
    dir: PathBuf,
    trace: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "regression-final-worker-bridge-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let trace = dir.join("trace.jsonl");
        Self { dir, trace }
    }
    fn server(&self, block: bool) -> StdioToolServer {
        // Fixture implementations use only their OS's standard shell. A blocked
        // call remains in stdin read (not a long sleep) until core kills the child.
        #[cfg(windows)]
        let (command, args) = {
            let script = self.dir.join("fixture.ps1");
            fs::write(&script, r#"
$ErrorActionPreference = 'Stop'
function Trace($event, $method) {
    $r = @{pid=$PID;event=$event;method=$method} | ConvertTo-Json -Compress
    [IO.File]::AppendAllText($env:RPI_BRIDGE_TRACE, $r + "`n")
}
Trace 'start' ''
$firstCall = $true
while ($null -ne ($line = [Console]::ReadLine())) {
    $req = $line | ConvertFrom-Json
    if ($null -eq $req.id) { continue }
    Trace 'request' $req.method
    if ($req.method -eq 'tools/call' -and $env:RPI_BRIDGE_BLOCK -eq 'yes') { continue }
    switch ($req.method) {
        'initialize' { $result = @{protocolVersion='2025-06-18';capabilities=@{tools=@{}};serverInfo=@{name='bridge-fixture';version='1'}} }
        'tools/list' { $result = @{tools=@(@{name='echo';inputSchema=@{type='object'}})} }
        'tools/call' {
            $result = @{content=@(@{type='text';text='actual child'});structuredContent=@{pid=$PID}}
            if ($firstCall -and $env:RPI_BRIDGE_BAD_FIRST) {
                $result = [IO.File]::ReadAllText($env:RPI_BRIDGE_BAD_FIRST) | ConvertFrom-Json
            }
            $firstCall = $false
        }
        default { $result = @{} }
    }
    $reply = @{jsonrpc='2.0';id=$req.id;result=$result} | ConvertTo-Json -Depth 20 -Compress
    [Console]::WriteLine($reply)
}
"#).unwrap();
            (
                "powershell.exe".into(),
                vec![
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    script.to_str().unwrap().into(),
                ],
            )
        };
        #[cfg(unix)]
        let (command, args) = {
            let script = self.dir.join("fixture.sh");
            fs::write(&script, r#"
printf '{"pid":%s,"event":"start","method":""}\n' "$$" >> "$RPI_BRIDGE_TRACE"
first_call=yes
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([^,}]*\).*/\1/p')
    [ -n "$id" ] || continue
    method=$(printf '%s\n' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    printf '{"pid":%s,"event":"request","method":"%s"}\n' "$$" "$method" >> "$RPI_BRIDGE_TRACE"
    case "$method" in
        initialize) result='{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"bridge-fixture","version":"1"}}' ;;
        tools/list) result='{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}' ;;
        tools/call)
            [ "$RPI_BRIDGE_BLOCK" = yes ] && continue
            result="{\"content\":[{\"type\":\"text\",\"text\":\"actual child\"}],\"structuredContent\":{\"pid\":$$}}"
            if [ "$first_call" = yes ] && [ -n "$RPI_BRIDGE_BAD_FIRST" ]; then
                result=$(cat "$RPI_BRIDGE_BAD_FIRST")
            fi
            first_call=no ;;
        *) result='{}' ;;
    esac
    printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
done
"#).unwrap();
            ("sh".into(), vec![script.to_str().unwrap().into()])
        };
        StdioToolServer {
            name: "fixture".into(),
            command,
            args,
            env: BTreeMap::from([
                (
                    "RPI_BRIDGE_TRACE".into(),
                    self.trace.to_str().unwrap().into(),
                ),
                (
                    "RPI_BRIDGE_BLOCK".into(),
                    if block { "yes" } else { "no" }.into(),
                ),
                (
                    "RPI_BRIDGE_BAD_FIRST".into(),
                    if self.dir.join("bad-first.json").exists() {
                        self.dir.join("bad-first.json").to_str().unwrap().into()
                    } else {
                        String::new()
                    },
                ),
            ]),
        }
    }
    fn records(&self) -> Vec<Value> {
        fs::read_to_string(&self.trace)
            .unwrap_or_default()
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
    fn pid(&self) -> u32 {
        self.records()
            .into_iter()
            .find(|r| r["event"] == "start")
            .unwrap()["pid"]
            .as_u64()
            .unwrap() as u32
    }
    fn count(&self, method: &str) -> usize {
        self.records()
            .iter()
            .filter(|r| r["event"] == "request" && r["method"] == method)
            .count()
    }
    async fn wait_call(&self) {
        let end = Instant::now() + WINDOW;
        while self.count("tools/call") == 0 {
            assert!(Instant::now() < end, "actual child did not receive call");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            fs::remove_dir_all(&self.dir).unwrap();
        } else {
            eprintln!("fixture artifacts: {}", self.dir.display());
        }
    }
}
fn native() -> (Arc<HostSession>, TestEvents, BufReader<DuplexStream>) {
    let (writer, peer) = tokio::io::duplex(65536);
    let (host, events) = HostSession::test_connection_with_ready(
        writer,
        2,
        json!({"type":"ready","protocol":"rpi","supportedCommands":["configure_external_tools","external_tool_response"],"supportedEvents":["external_tool_request","external_tool_request_closed"]}),
    );
    (host, events, BufReader::new(peer))
}
async fn frame(peer: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(WINDOW, peer.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn install(
    f: &Fixture,
    block: bool,
    host: &HostSession,
    events: &TestEvents,
    peer: &mut BufReader<DuplexStream>,
) -> String {
    let configure = host.configure_stdio_tools(
        vec![f.server(block)],
        f.dir.clone(),
        "bridge-fixture".into(),
    );
    let native = async {
        let config = frame(peer).await;
        assert_eq!(config["type"], "configure_external_tools");
        assert_eq!(config["tools"].as_array().unwrap().len(), 1);
        let name = config["tools"][0]["name"].as_str().unwrap().to_owned();
        events
            .send(
                json!({"type":"response","id":config["id"],"success":true,"data":{"tools":[name]}}),
            )
            .ok();
        name
    };
    let (result, name) = tokio::join!(configure, native);
    result.unwrap();
    name
}
fn invoke(events: &TestEvents, tool: &str, id: &str) {
    events
        .send(json!({"type":"external_tool_request","requestId":id,"toolName":tool,"arguments":{}}))
        .ok();
}
#[cfg(windows)]
fn running(pid: u32) -> bool {
    let status = std::process::Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}")]).status().unwrap();
    assert!(
        matches!(status.code(), Some(0 | 1)),
        "process probe failed: {status}"
    );
    status.success()
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
async fn exited(pid: u32) {
    tokio::time::timeout(WINDOW, async {
        while tokio::task::spawn_blocking(move || running(pid))
            .await
            .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("actual MCP process survived bridge cancellation");
}
async fn no_response(peer: &mut BufReader<DuplexStream>) {
    let mut line = String::new();
    match tokio::time::timeout(Duration::from_millis(200), peer.read_line(&mut line)).await {
        Err(_) | Ok(Ok(0)) => {}
        other => panic!("closed request emitted a late frame: {other:?} {line}"),
    }
}

#[tokio::test]
async fn request_closed_cancels_actual_blocked_call_without_late_response_or_reconnect() {
    let f = Fixture::new();
    let (host, events, mut peer) = native();
    let tool = install(&f, true, &host, &events, &mut peer).await;
    invoke(&events, &tool, "closed");
    f.wait_call().await;
    let pid = f.pid();
    assert!(running(pid));
    events
        .send(json!({"type":"external_tool_request_closed","requestId":"closed"}))
        .ok();
    exited(pid).await;
    no_response(&mut peer).await;
    // A still-configured bridge must not silently reconnect its retired target.
    invoke(&events, &tool, "next");
    let response = frame(&mut peer).await;
    assert_eq!(response["requestId"], "next");
    assert_eq!(response["error"], "external tool call failed");
    events
        .send(
            json!({"type":"response","id":response["id"],"success":true,"data":{"accepted":true}}),
        )
        .ok();
    events
        .send(json!({"type":"external_tool_request_closed","requestId":"next"}))
        .ok();
    assert_eq!(
        f.records().iter().filter(|r| r["event"] == "start").count(),
        1
    );
    assert_eq!(f.count("tools/list"), 1);
    host.terminate().await;
}

#[tokio::test]
async fn last_session_owner_drop_cancels_actual_blocked_call_and_releases_session() {
    let f = Fixture::new();
    let (host, events, mut peer) = native();
    let tool = install(&f, true, &host, &events, &mut peer).await;
    invoke(&events, &tool, "owner-drop");
    f.wait_call().await;
    let pid = f.pid();
    assert!(running(pid));
    let weak = Arc::downgrade(&host);
    // The test injector intentionally owns a session Arc too.
    drop(events);
    drop(host);
    assert!(
        weak.upgrade().is_none(),
        "bridge task retained owning session"
    );
    exited(pid).await;
    no_response(&mut peer).await;
}

#[tokio::test]
async fn actual_child_result_routes_without_broadcast_subscribers_or_list_requery() {
    let f = Fixture::new();
    let (host, events, mut peer) = native();
    let tool = install(&f, false, &host, &events, &mut peer).await;
    for id in ["one", "two"] {
        invoke(&events, &tool, id);
        let response = frame(&mut peer).await;
        assert_eq!(response["requestId"], id);
        assert_eq!(response["result"]["content"][0]["text"], "actual child");
        assert_eq!(response["result"]["structuredContent"]["pid"], f.pid());
        events.send(json!({"type":"response","id":response["id"],"success":true,"data":{"accepted":true}})).ok();
        events
            .send(json!({"type":"external_tool_request_closed","requestId":id}))
            .ok();
    }
    assert_eq!(f.count("initialize"), 1);
    assert_eq!(f.count("tools/list"), 1);
    assert_eq!(f.count("tools/call"), 2);
    let pid = f.pid();
    host.terminate().await;
    exited(pid).await;
}

fn acknowledge_and_close(events: &TestEvents, response: &Value, id: &str) {
    assert_eq!(response["type"], "external_tool_response");
    assert_eq!(response["requestId"], id);
    assert!(
        response["id"].is_string(),
        "missing native command correlation"
    );
    // TestEvents routes reliably before optional broadcast delivery. These
    // fixtures intentionally have no broadcast subscribers; its send error is
    // not the routing outcome. The following call proves the host stays live.
    events
        .send(
            json!({"type":"response","id":response["id"],"success":true,"data":{"accepted":true}}),
        )
        .ok();
    events
        .send(json!({"type":"external_tool_request_closed","requestId":id}))
        .ok();
}

#[tokio::test]
async fn actual_blocked_call_budget_times_out_without_stopping_session_or_reconnecting() {
    let f = Fixture::new();
    let (host, events, mut peer) = native();
    let tool = install(&f, true, &host, &events, &mut peer).await;
    invoke(&events, &tool, "budget");
    f.wait_call().await;
    let pid = f.pid();
    assert!(
        running(pid),
        "timeout must exercise a live blocked MCP child"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(46)).await;
    // Resume before reading frames: cleanup joins a real blocking worker and
    // must not race virtual auto-advance of the frame helper's WINDOW.
    tokio::time::resume();
    tokio::task::yield_now().await;
    let response = frame(&mut peer).await;
    assert_eq!(response["error"], "external tool call timed out");
    assert!(response.get("result").is_none());
    acknowledge_and_close(&events, &response, "budget");
    exited(pid).await;

    invoke(&events, &tool, "after-budget");
    let response = frame(&mut peer).await;
    assert_eq!(response["error"], "external tool call failed");
    assert!(response.get("result").is_none());
    acknowledge_and_close(&events, &response, "after-budget");
    assert_eq!(
        f.records().iter().filter(|r| r["event"] == "start").count(),
        1
    );
    assert_eq!(f.count("initialize"), 1);
    assert_eq!(f.count("tools/list"), 1);
    assert_eq!(f.count("tools/call"), 1);
    host.terminate().await;
}

async fn actual_first_result(first: Value, expected_error: bool) {
    let f = Fixture::new();
    fs::write(f.dir.join("bad-first.json"), first.to_string()).unwrap();
    let (host, events, mut peer) = native();
    let tool = install(&f, false, &host, &events, &mut peer).await;
    invoke(&events, &tool, "first-result");
    let response = frame(&mut peer).await;
    let pid = f.pid();
    assert!(running(pid));
    if expected_error {
        assert_eq!(response["error"], "unsupported external tool content block");
        assert!(response.get("result").is_none());
    } else {
        assert!(response.get("error").is_none());
        assert_eq!(
            response["result"], first,
            "valid raw result metadata changed"
        );
    }
    acknowledge_and_close(&events, &response, "first-result");

    invoke(&events, &tool, "valid-after-first");
    let response = frame(&mut peer).await;
    assert!(response.get("error").is_none());
    assert_eq!(response["result"]["content"][0]["text"], "actual child");
    assert_eq!(response["result"]["structuredContent"]["pid"], pid);
    acknowledge_and_close(&events, &response, "valid-after-first");
    assert_eq!(
        f.records().iter().filter(|r| r["event"] == "start").count(),
        1
    );
    assert_eq!(f.count("initialize"), 1);
    assert_eq!(f.count("tools/list"), 1);
    assert_eq!(f.count("tools/call"), 2);
    assert!(
        running(pid),
        "result validation must not retire a usable child"
    );
    host.terminate().await;
    exited(pid).await;
}

#[tokio::test]
async fn actual_invalid_base64_result_is_rejected_and_session_stays_usable() {
    actual_first_result(
        json!({"content":[{"type":"image","data":"%%%not-base64%%%","mimeType":"image/png"}]}),
        true,
    )
    .await;
}

#[tokio::test]
async fn actual_integer_text_signature_is_rejected_and_session_stays_usable() {
    actual_first_result(
        json!({"content":[{"type":"text","text":"bad signature","textSignature":42}]}),
        true,
    )
    .await;
}

#[tokio::test]
async fn actual_unsupported_content_is_rejected_and_session_stays_usable() {
    actual_first_result(
        json!({"content":[{"type":"audio","data":"aGk=","mimeType":"audio/wav"}]}),
        true,
    )
    .await;
}

#[tokio::test]
async fn actual_raw_image_and_text_metadata_are_preserved_and_session_stays_usable() {
    actual_first_result(
        json!({"content":[
            {"type":"image","data":"aGk=","mimeType":"image/png","annotations":{"audience":["user"],"priority":0.5},"_meta":{"fixture":"image"}},
            {"type":"text","text":"signed","textSignature":"fixture-signature","_meta":{"fixture":"text"}},
            {"type":"text","text":"unsigned","textSignature":null}
        ],"structuredContent":{"nested":{"preserved":[1,true,"raw"]}},"isError":false,"_meta":{"fixture":"result"}}),
        false,
    ).await;
}
