//! Fake executor wire proofs only: no native agent/model RPC is installed here.
use rpi_mcp_core::{sampling::AuthorizationFuture, *};
use serde_json::{json, Value};
use std::{io::{Read, Write}, net::{TcpListener, TcpStream}, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
struct Provider { deny: bool, delay: bool, seen: Mutex<Vec<SamplingContext>> }
impl SamplingAuthorizer for Provider {
    fn authorize<'a>(&'a self, c: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(c.clone());
            if self.deny { return Err(SamplingError::Denied); }
            Ok(SamplingPolicy { context:c.clone(), max_messages:128, max_tokens:16384, max_duration:Duration::from_secs(300) })
        })
    }
}
impl SamplingExecutor for Provider {
    fn execute<'a>(&'a self, _: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> SamplingFuture<'a> {
        Box::pin(async move {
            if self.delay { tokio::time::sleep(Duration::from_secs(20)).await; }
            SamplingResult::parse(json!({"role":"assistant","content":{"type":"text","text":"fake-wire-result"},"model":"fake-wire-model","stopReason":"endTurn"}))
        })
    }
}
fn provider(deny: bool, delay: bool) -> Arc<Provider> { Arc::new(Provider { deny, delay, seen:Mutex::new(vec![]) }) }
fn runtime(p: &Arc<Provider>) -> McpRuntime { McpRuntime::with_providers(None, Some(SamplingOptions { executor:p.clone(), authorizer:p.clone() })) }
struct Fixture { dir: PathBuf, context: RequestContext }
impl Fixture {
    fn new() -> Self {
        static ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = PathBuf::from("C:/Users/bigfish/.cargo/target/acp-tmp").join(format!("sampling-wire-{}-{}",std::process::id(),ID.fetch_add(1,std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mcp.json"), json!({"mcpServers":{"sample":{"command":env!("CARGO_BIN_EXE_mcp-catalog-fixture"),"args":[dir.join("capture.jsonl")],"timeout":3}}}).to_string()).unwrap();
        Self { context:RequestContext { scope:"host-session".into(),cwd:dir.clone() },dir }
    }
    fn call(&self, runtime:&McpRuntime, name:&str, control:&Control) -> Result<Value,String> {
        runtime.call(&self.context,&json!({"configPath":"mcp.json","server":"sample","tool":name}),control)
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); } }
fn payload(v: Value) -> Value {
    // Runtime call exposes both structured and text result; locate fixture JSON.
    let text = v["content"][0]["text"].as_str().or_else(||v["text"].as_str());
    if let Some(text) = text { if let Ok(v) = serde_json::from_str(text) { return v; } }
    if v.get("result").is_some() { return payload(v["result"].clone()); }
    panic!("unexpected fixture result: {v}");
}
#[test]
fn stdio_sampling_success_and_bound_identity() {
    let f=Fixture::new(); let p=provider(false,false); let r=runtime(&p);
    let v=payload(f.call(&r,"success",&Control::default()).unwrap());
    assert_eq!(v["callback"]["id"],"sample-01");
    assert_eq!(v["callback"]["result"]["model"],"fake-wire-model");
    assert_eq!(v["advertised"],json!({"sampling":{}}));
    let seen=p.seen.lock().unwrap(); let c=&seen[0];
    assert_eq!(c.scope,"host-session"); assert_eq!(c.cwd,std::fs::canonicalize(&f.dir).unwrap());
    assert_eq!(c.server,"sample"); assert!(c.operation.starts_with("tools/call:")); assert_eq!(c.namespace.len(),64);
}
#[test]
fn stdio_default_does_not_advertise_or_execute() {
    let f=Fixture::new(); let r=McpRuntime::new();
    let v=payload(f.call(&r,"success",&Control::default()).unwrap());
    assert_eq!(v["advertised"],json!({})); assert_eq!(v["callback"]["error"]["code"],-32601);
}
#[test]
fn stdio_denial_unsupported_and_malformed_are_redacted() {
    for (scenario,deny,code) in [("success",true,-32603),("context",false,-32602),("malformed",false,-32600)] {
        let f=Fixture::new(); let p=provider(deny,false); let r=runtime(&p);
        let v=payload(f.call(&r,scenario,&Control::default()).unwrap());
        assert_eq!(v["callback"]["error"]["code"],code);
        assert!(!v.to_string().contains("PRIVATE-REMOTE-PAYLOAD"));
        if scenario=="malformed" { assert!(v["callback"]["id"].is_null()); }
    }
}
#[test]
fn stdio_exact_server_cancellation() {
    let f=Fixture::new(); let p=provider(false,true); let r=runtime(&p);
    let v=payload(f.call(&r,"cancel",&Control::default()).unwrap());
    assert_eq!(v["callback"]["error"]["message"],"Sampling cancelled");
}
#[test]
fn stdio_host_cancel_and_deadline_join_workers() {
    for cancel in [true,false] {
        let f=Fixture::new(); let p=provider(false,true); let r=runtime(&p); let control=Control::default();
        let thread=if cancel { let control=control.clone(); Some(std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(150)); control.cancel(); })) } else { None };
        let start=std::time::Instant::now(); assert!(f.call(&r,"success",&control).is_err());
        drop(r); assert!(start.elapsed()<Duration::from_secs(6)); if let Some(t)=thread { t.join().unwrap(); }
    }
}
fn read_request(s:&mut TcpStream) -> Value {
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bytes=Vec::new(); let mut b=[0];
    while !bytes.ends_with(b"\r\n\r\n") { s.read_exact(&mut b).unwrap(); bytes.push(b[0]); }
    let headers=String::from_utf8(bytes).unwrap();
    let len:usize=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap();
    let mut body=vec![0;len]; s.read_exact(&mut body).unwrap(); {
        let value:Value=serde_json::from_slice(&body).unwrap();
        if value["method"] != "initialize" {
            let headers=headers.to_ascii_lowercase();
            assert!(headers.contains("mcp-session-id: sampling-session"));
            assert!(headers.contains("mcp-protocol-version: 2025-06-18"));
        }
        value
    }
}
fn response(s:&mut TcpStream, status:&str, body:&str, extra:&str) { write!(s,"HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{extra}\r\n{body}",body.len()).unwrap(); s.flush().unwrap(); }
#[test]
fn http_sse_reverse_sampling_while_parent_waits() {
    for deny in [false,true] {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap(); let url=format!("http://{}/mcp",listener.local_addr().unwrap());
        let server=std::thread::spawn(move || {
            let (mut s,_)=listener.accept().unwrap(); let init=read_request(&mut s); assert_eq!(init["params"]["capabilities"],json!({"sampling":{}}));
            response(&mut s,"200 OK",&json!({"jsonrpc":"2.0","id":init["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"http","version":"1"}}}).to_string(),"Mcp-Session-Id: sampling-session\r\n");
            let (mut s,_)=listener.accept().unwrap(); let _=read_request(&mut s); response(&mut s,"202 Accepted","","");
            let (mut s,_)=listener.accept().unwrap(); let list=read_request(&mut s);
            response(&mut s,"200 OK",&json!({"jsonrpc":"2.0","id":list["id"],"result":{"tools":[{"name":"sample","inputSchema":{"type":"object"}}]}}).to_string(),"");
            let (mut stream,_)=listener.accept().unwrap(); let parent=read_request(&mut stream); assert_eq!(parent["method"],"tools/call");
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {}\n\n",json!({"jsonrpc":"2.0","id":-7,"method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"PRIVATE-REMOTE-PAYLOAD"}}],"maxTokens":20}})).unwrap(); stream.flush().unwrap();
            let (mut s,_)=listener.accept().unwrap(); let callback=read_request(&mut s); assert_eq!(callback["id"],-7);
            assert!(!callback.to_string().contains("PRIVATE-REMOTE-PAYLOAD"));
            if deny { assert_eq!(callback["error"]["message"],"Sampling denied"); } else { assert_eq!(callback["result"]["model"],"fake-wire-model"); }
            response(&mut s,"202 Accepted","","");
            write!(stream,"data: {}\n\n",json!({"jsonrpc":"2.0","id":parent["id"],"result":{"content":[{"type":"text","text":"done"}]}})).unwrap(); stream.flush().unwrap();
        });
        let p=provider(deny,false); let r=runtime(&p);
        r.call(&RequestContext::default(),&json!({"url":url,"tool":"sample"}),&Control::default()).unwrap();
        server.join().unwrap();
    }
}
