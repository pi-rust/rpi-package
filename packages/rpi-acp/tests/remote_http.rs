use rpi_acp::transport::{http, Config};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpListener, sync::watch};
const TOKEN: &str = "remote-test-secret-0123456789abcdef";
fn client() -> Client { Client::builder().http2_prior_knowledge().timeout(Duration::from_secs(15)).build().unwrap() }
async fn start() -> (SocketAddr, watch::Sender<bool>, tokio::task::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop,rx) = watch::channel(false);
    let task = tokio::spawn(http::serve(listener,Config::new(TOKEN,Some("https://editor.example".into())).unwrap(),rx));
    (address,stop,task)
}
fn post(c: &Client, a: SocketAddr, id: Option<&str>, session: Option<&str>, value: Value) -> reqwest::RequestBuilder {
    let mut r = c.post(format!("http://{a}/acp")).bearer_auth(TOKEN).json(&value);
    if let Some(id) = id { r = r.header("acp-connection-id",id); }
    if let Some(s) = session { r = r.header("acp-session-id",s); }
    r
}
async fn init(c: &Client, a: SocketAddr) -> String {
    init_with_capabilities(c,a,json!({})).await
}
async fn init_with_capabilities(c: &Client, a: SocketAddr, capabilities: Value) -> String {
    let r = post(c,a,None,None,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":capabilities}})).send().await.unwrap();
    assert_eq!(r.status(),200);
    let id = r.headers()["acp-connection-id"].to_str().unwrap().to_owned();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["result"]["connectionId"],id);
    assert_eq!(v["result"]["protocolVersion"],1);
    id
}
async fn stream(c: &Client,a:SocketAddr,id:&str,s:Option<&str>) -> Response {
    let mut r = c.get(format!("http://{a}/acp")).bearer_auth(TOKEN).header("accept","text/event-stream").header("acp-connection-id",id);
    if let Some(s) = s { r = r.header("acp-session-id",s); }
    let r = r.send().await.unwrap(); assert_eq!(r.status(),200); r
}
struct Events { response: Response, buffer: Vec<u8> }
impl Events {
    fn new(response: Response) -> Self { Self { response, buffer: Vec::new() } }
    async fn next(&mut self) -> Value {
        loop {
            if let Some(end) = self.buffer.windows(2).position(|w| w == b"\n\n") {
                let event = String::from_utf8(self.buffer[..end].to_vec()).unwrap(); self.buffer.drain(..end+2);
                assert!(!event.lines().any(|l| l.starts_with("id:")),"no fake replay cursor");
                if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) { return serde_json::from_str(data).unwrap(); }
                continue;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10),self.response.chunk()).await.unwrap().unwrap().expect("unexpected SSE EOF");
            // Accumulate bytes as UTF-8 only after a complete event; reqwest/h2
            // may split a Unicode codepoint across DATA frames.
            self.buffer.extend_from_slice(&chunk);
        }
    }
}
#[tokio::test]
async fn security_negotiation_identity_and_connection_lifecycle() {
    let (a,stop,task) = start().await;
    let c = client(); let url = format!("http://{a}/acp");
    assert_eq!(c.get(&url).send().await.unwrap().status(),401);
    for (header,value,status) in [("authorization","Bearer wrong",401),("origin","null",403),("origin","https://evil.example",403),("host","evil.example",403),("last-event-id","12",501),("acp-protocol-version","1",501)] {
        assert_eq!(c.get(&url).bearer_auth(TOKEN).header(header,value).send().await.unwrap().status(),status,"{header}");
    }
    assert_eq!(post(&c,a,None,None,json!([])).send().await.unwrap().status(),501);
    assert_eq!(post(&c,a,None,None,json!({"jsonrpc":"2.0","id":2,"method":"unknown"})).send().await.unwrap().status(),400);
    assert_eq!(c.post(&url).bearer_auth(TOKEN).body("{}").send().await.unwrap().status(),415);
    assert_eq!(post(&c,a,None,None,json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":0}})).send().await.unwrap().status(),200);
    for params in [json!({"protocolVersion":0}),json!({"protocolVersion":2,"info":{"name":"client","version":"1"},"capabilities":{}})] {
        let r = post(&c,a,None,None,json!({"jsonrpc":"2.0","id":7,"method":"initialize","params":params})).send().await.unwrap();
        assert_eq!(r.status(),200); assert_eq!(r.version(),reqwest::Version::HTTP_2); assert!(!r.headers().contains_key("acp-connection-id"));
        let v:Value=r.json().await.unwrap(); assert_eq!(v["id"],7); assert_eq!(v["error"]["code"],-32602); assert!(v.get("result").is_none());
    }
    let id = init(&c,a).await;
    let other = client();
    assert_eq!(other.get(&url).bearer_auth(TOKEN).header("accept","text/event-stream").header("acp-connection-id",&id).send().await.unwrap().status(),404,"connection ID cannot attach from another TCP client");
    assert_eq!(c.get(&url).bearer_auth(TOKEN).header("acp-connection-id",&id).send().await.unwrap().status(),406);
    let mut events = Events::new(stream(&c,a,&id,None).await);
    assert_eq!(c.get(&url).bearer_auth(TOKEN).header("accept","text/event-stream").header("acp-connection-id",&id).send().await.unwrap().status(),409);
    assert_eq!(c.get(&url).bearer_auth(TOKEN).header("accept","text/event-stream").header("acp-connection-id",&id).header("acp-session-id","unknown").send().await.unwrap().status(),404);
    assert_eq!(c.delete(&url).bearer_auth(TOKEN).header("acp-connection-id",&id).header("acp-session-id","unknown").send().await.unwrap().status(),400);
    let value = json!({"jsonrpc":"2.0","id":2,"method":"unknown"});
    let r = post(&c,a,Some(&id),None,value.clone()).send().await.unwrap(); assert_eq!(r.status(),202); assert!(r.bytes().await.unwrap().is_empty());
    assert_eq!(events.next().await["id"],2);
    assert_eq!(post(&c,a,Some(&id),None,value).send().await.unwrap().status(),400,"IDs cannot be retried as fresh requests");
    assert_eq!(post(&c,a,Some(&id),None,json!({"jsonrpc":"2.0","method":"initialize","id":3})).send().await.unwrap().status(),400);
    assert_eq!(c.delete(&url).bearer_auth(TOKEN).header("acp-connection-id",&id).send().await.unwrap().status(),202);
    drop(events);
    let id2 = init(&c,a).await; assert_ne!(id,id2);
    let events = stream(&c,a,&id2,None).await; drop(events);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(c.delete(&url).bearer_auth(TOKEN).header("acp-connection-id",&id2).send().await.unwrap().status(),404);
    stop.send(true).unwrap(); tokio::time::timeout(Duration::from_secs(10),task).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn native_stream_cancel_eof_teardown_and_persisted_reconnect() {
    let bridge = std::path::PathBuf::from(env!("CARGO_BIN_EXE_rpi-acp-bridge"));
    let fake = bridge.parent().unwrap().join("examples").join(format!("remote_rpc{}",std::env::consts::EXE_SUFFIX));
    assert!(fake.exists(),"build --example remote_rpc first");
    let directory = std::env::temp_dir().join(format!("rpi-http-{}",uuid::Uuid::new_v4())); std::fs::create_dir_all(&directory).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); let a=listener.local_addr().unwrap(); drop(listener);
    let pids = directory.join("pids");
    let mut child = tokio::process::Command::new(bridge).args(["--experimental-http","--bind",&a.to_string()]).env("RPI_ACP_REMOTE_TOKEN",TOKEN).env("RPI_BIN",fake).env("RPI_ACP_SESSION_DIR",directory.join("history")).env("RPI_REMOTE_TEST_PIDS",&pids).env("RPI_REMOTE_TEST_EARLY_UPDATE","1").env("RPI_EXTENSIONS_DIR",&directory).env("RPI_CODING_AGENT_DIR",&directory).env("RPI_ACP_NO_AUTO_REGISTER_ZED","1").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).kill_on_drop(true).spawn().unwrap();
    for _ in 0..200 { if tokio::net::TcpStream::connect(a).await.is_ok() { break; } tokio::time::sleep(Duration::from_millis(25)).await; }
    let c=client(); let id=init(&c,a).await;
    let mut conn=Events::new(stream(&c,a,&id,None).await);
    assert_eq!(post(&c,a,Some(&id),None,json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":directory,"mcpServers":[]}})).send().await.unwrap().status(),202);
    let response=conn.next().await; let session=response["result"]["sessionId"].as_str().unwrap_or_else(|| panic!("{response}")).to_owned();
    let mut events=Events::new(stream(&c,a,&id,Some(&session)).await);
    // session/new emits this before its response, when the transport has not
    // learned the ID yet. It must be retained for a later session GET, once.
    let early=events.next().await;
    assert_eq!(early["method"],"session/update");
    assert_eq!(early["params"]["sessionId"],session);
    assert_eq!(early["params"]["update"]["sessionUpdate"],"available_commands_update");
    assert_eq!(early["params"]["update"]["availableCommands"][0]["name"],"early");
    assert_eq!(post(&c,a,Some(&id),Some("wrong"),json!({"jsonrpc":"2.0","id":30,"method":"session/prompt","params":{"sessionId":session,"prompt":[]}})).send().await.unwrap().status(),400);
    for (rid,text,reason) in [(3,"complete","end_turn"),(4,"wait","cancelled")] {
        assert_eq!(post(&c,a,Some(&id),Some(&session),json!({"jsonrpc":"2.0","id":rid,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":text}]}})).send().await.unwrap().status(),202);
        let mut streamed=false;
        loop { let v=events.next().await;
            assert_ne!(v["params"]["update"]["sessionUpdate"],"available_commands_update","early notification delivered more than once");
            if v["params"]["update"]["sessionUpdate"]=="agent_message_chunk" { streamed=true; assert_eq!(v["params"]["update"]["content"]["text"],"remote🌊"); if text=="wait" { assert_eq!(post(&c,a,Some(&id),Some(&session),json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})).send().await.unwrap().status(),202); } }
            if v["id"]==rid { assert!(streamed); assert_eq!(v["result"]["stopReason"],reason,"{v}"); break; }
        }
    }
    post(&c,a,Some(&id),Some(&session),json!({"jsonrpc":"2.0","id":5,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":"wait"}]}})).send().await.unwrap();
    events.next().await; drop(events); drop(conn); drop(c);
    tokio::time::timeout(Duration::from_secs(10),async { loop { if std::fs::read_to_string(&pids).is_ok_and(|s| s.lines().all(|p| !process_alive(p))) { break; } tokio::time::sleep(Duration::from_millis(100)).await; } }).await.expect("native process leaked after HTTP EOF");
    // GET reserves validated history but must not spawn a native child.
    let before = std::fs::read_to_string(&pids).unwrap();
    let c=client(); let id=init(&c,a).await; let mut conn=Events::new(stream(&c,a,&id,None).await);
    let reserved=stream(&c,a,&id,Some(&session)).await;
    assert_eq!(std::fs::read_to_string(&pids).unwrap(),before);
    // GET alone reserves history; it does not attach it for agent operations.
    assert_eq!(post(&c,a,Some(&id),Some(&session),json!({"jsonrpc":"2.0","id":31,"method":"session/prompt","params":{"sessionId":session,"prompt":[]}})).send().await.unwrap().status(),400);
    let other=client(); let other_id=init(&other,a).await;
    assert_eq!(other.get(format!("http://{a}/acp")).bearer_auth(TOKEN).header("accept","text/event-stream").header("acp-connection-id",&other_id).header("acp-session-id",&session).send().await.unwrap().status(),409,"exclusive resume claim");
    assert_eq!(post(&c,a,Some(&id),Some(&session),json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":session,"cwd":"relative-invalid","mcpServers":[]}})).send().await.unwrap().status(),202);
    let failure=conn.next().await; assert_eq!(failure["id"],2); assert!(failure.get("error").is_some(),"{failure}");
    // The failed attachment must release the claim even with its old SSE body alive.
    let reserved_other=stream(&other,a,&other_id,Some(&session)).await;
    assert_eq!(std::fs::read_to_string(&pids).unwrap(),before);
    assert_eq!(other.delete(format!("http://{a}/acp")).bearer_auth(TOKEN).header("acp-connection-id",&other_id).header("acp-session-id",&session).send().await.unwrap().status(),400,"DELETE is connection-scoped even for a known session");
    assert_eq!(other.delete(format!("http://{a}/acp")).bearer_auth(TOKEN).header("acp-connection-id",&other_id).send().await.unwrap().status(),202);
    drop(reserved_other); drop(other); drop(reserved); drop(conn); drop(c);
    // DELETE replies before asynchronous cleanup; wait for the claim to be released.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let c=client(); let id=init_with_capabilities(&c,a,json!({"_meta":{"rpi/unstable-session-rfds":"1142c83bbfa9407f9f264d1e290ffafcd54eb79e"}})).await;
    let mut conn=Events::new(stream(&c,a,&id,None).await);
    let mut events=Events::new(stream(&c,a,&id,Some(&session)).await);
    assert_eq!(post(&c,a,Some(&id),Some(&session),json!({"jsonrpc":"2.0","id":6,"method":"session/load","params":{"sessionId":session,"cwd":directory,"mcpServers":[]}})).send().await.unwrap().status(),202);
    let v=conn.next().await; assert_eq!(v["id"],6); assert!(v.get("result").is_some(),"{v}");
    let mut replay=Vec::new();
    while replay.len()<2 { let v=events.next().await; if matches!(v["params"]["update"]["sessionUpdate"].as_str(),Some("user_message_chunk"|"agent_message_chunk")) { replay.push(v["params"]["update"]["content"]["text"].clone()); } }
    assert_eq!(replay,vec![json!("complete"),json!("remote🌊")]);
    // Regression: load results omit sessionId; the transport must still mark
    // the pending saved-session attachment successful before scoped operations.
    let source_file = rpi_extcore::history::find_saved_session(&directory.join("history").join(&session), &session).unwrap();
    let source_bytes = std::fs::read(&source_file).unwrap();
    let fork=json!({"jsonrpc":"2.0","id":7,"method":"session/fork","params":{"sessionId":session,"cwd":directory,"mcpServers":[]}});
    assert_eq!(post(&c,a,Some(&id),None,fork.clone()).send().await.unwrap().status(),400,"fork needs source header");
    assert_eq!(post(&c,a,Some(&id),Some("wrong"),fork.clone()).send().await.unwrap().status(),400);
    assert_eq!(post(&c,a,Some(&id),Some(&session),fork).send().await.unwrap().status(),202);
    let forked=conn.next().await; assert_eq!(forked["id"],7);
    let fork_id=forked["result"]["sessionId"].as_str().unwrap_or_else(|| panic!("{forked}")); assert_ne!(fork_id,session);
    assert_eq!(uuid::Uuid::parse_str(fork_id).unwrap().to_string(), fork_id);
    let fork_file = rpi_extcore::history::find_saved_session(&directory.join("history").join(fork_id), fork_id).unwrap();
    let fork_bytes = std::fs::read(&fork_file).unwrap();
    let rows = |bytes: &[u8]| -> Vec<Value> {
        std::str::from_utf8(bytes).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    };
    let source_rows = rows(&source_bytes);
    let fork_rows = rows(&fork_bytes);
    assert_eq!(fork_rows[0]["id"], fork_id);
    assert_eq!(fork_rows[0]["parentSessionId"], session);
    assert_eq!(&fork_rows[1..], &source_rows[1..], "fork preserves persisted native rows");
    assert_eq!(std::fs::read(&source_file).unwrap(), source_bytes, "fork must not mutate source");
    let fork_stream=stream(&c,a,&id,Some(fork_id)).await;
    drop(fork_stream); drop(events); drop(conn); drop(c);
    wait_for_native_exit(&pids).await;
    // No GET SSE exists: the accepted network socket still owns native cleanup.
    let c=client(); let id=init(&c,a).await;
    let before=std::fs::read_to_string(&pids).unwrap().lines().count();
    assert_eq!(post(&c,a,Some(&id),None,json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":directory,"mcpServers":[]}})).send().await.unwrap().status(),202);
    tokio::time::timeout(Duration::from_secs(10),async { loop {
        if std::fs::read_to_string(&pids).unwrap().lines().count()>before { break; }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }}).await.unwrap();
    drop(c);
    wait_for_native_exit(&pids).await;
    child.kill().await.unwrap(); let _=std::fs::remove_dir_all(directory);
}
async fn wait_for_native_exit(pids:&std::path::Path) {
    tokio::time::timeout(Duration::from_secs(8),async { loop {
        if std::fs::read_to_string(pids).is_ok_and(|s| s.lines().all(|p| !process_alive(p))) { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }}).await.expect("native process leaked after network EOF (before initial/idle deadline)");
}
fn process_alive(id:&str)->bool {
    #[cfg(windows)] { String::from_utf8_lossy(&std::process::Command::new("tasklist").args(["/FI",&format!("PID eq {id}"),"/FO","CSV","/NH"]).output().unwrap().stdout).contains(&format!("\"{id}\"")) }
    #[cfg(not(windows))] { std::process::Command::new("kill").args(["-0",id]).status().unwrap().success() }
}
