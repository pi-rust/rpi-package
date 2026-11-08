//! Opt-in frozen-native HTTP recovery acceptance, not live execution or UI parity.
use futures_util::FutureExt;
use rpi_extcore::{
    registry::{Registry, RegistryEntry},
    reply::ReplyKind,
    service::{
        self,
        tool_program_queries::{
            ChildBodyPage, ChildBodyQuery, ChildPage, ChildPageQuery, ChildPhase, ChildPhaseKind,
            ChildPhasePage, ChildPhaseQuery, Direction,
        },
        tool_program_snapshot::{ChildSnapshot, ChildSnapshotQuery},
        BackendConfig, Service,
    },
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Read},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    time::Duration,
};

fn digest(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    format!("{:x}", hash.finalize())
}
fn absolute(name: &str) -> PathBuf {
    let p = PathBuf::from(std::env::var_os(name).expect(name));
    assert!(p.is_absolute(), "{name} must be explicitly absolute");
    p
}
fn save(path: impl AsRef<Path>, value: &impl Serialize) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn primaries(root: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        assert!(!e.file_type().unwrap().is_symlink());
        if e.file_type().unwrap().is_dir() {
            primaries(&e.path(), out);
        } else if e.path().extension().is_some_and(|v| v == "jsonl")
            && !e.path().to_string_lossy().ends_with(".progress.jsonl")
        {
            out.push(e.path());
        }
    }
}
fn lines(path: &Path) -> Vec<Value> {
    BufReader::new(fs::File::open(path).unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}
// Independent tiny decoder keeps this opt-in integration target dependency-free.
fn decode(s: &str) -> Vec<u8> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let n = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("invalid base64"),
        };
        bits = (bits << 6) | n as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    out
}
fn execution(v: &Value) -> bool {
    matches!(
        v["type"].as_str(),
        Some(
            "agent_start"
                | "operation_accepted"
                | "tool_execution_start"
                | "tool_execution_update"
                | "tool_execution_end"
                | "message_start"
                | "message_update"
                | "external_tool_request"
                | "tool_program_child_begin"
                | "tool_program_child_phase"
        )
    )
}
struct Http {
    client: reqwest::Client,
    base: String,
    dir: PathBuf,
    calls: usize,
    max_response: usize,
}
impl Http {
    async fn post<Q: Serialize, T: DeserializeOwned>(
        &mut self,
        id: &str,
        route: &str,
        q: &Q,
        status: u16,
    ) -> T {
        let response = self
            .client
            .post(format!(
                "{}/api/v1/sessions/{id}/tool-program/{route}",
                self.base
            ))
            .json(q)
            .send()
            .await
            .unwrap();
        let actual = response.status().as_u16();
        let bytes = response.bytes().await.unwrap();
        self.max_response = self.max_response.max(bytes.len());
        save(
            self.dir.join(format!("http-{:04}.json", self.calls)),
            &json!({"route":route,"request":q,"status":actual,"bytes":bytes.len(),"response":serde_json::from_slice::<Value>(&bytes).unwrap()}),
        );
        self.calls += 1;
        assert_eq!(
            actual,
            status,
            "{route}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap()
    }
}
fn fresh() -> ChildSnapshotQuery {
    ChildSnapshotQuery {
        children: ChildPageQuery {
            leaf_id: None,
            cursor: None,
            direction: Direction::NewestFirst,
            limit: 8,
            max_bytes: 32768,
        },
        phase_limit: 2,
    }
}

#[tokio::test]
#[ignore = "requires absolute frozen native BIN, SHA256, source EVIDENCE and NEW HTTP_EVIDENCE_OUT; no model or PATH native"]
async fn actual_native_readonly_http_child_recovery() {
    let bin = absolute("RPI_CODEMODE_NATIVE_BIN");
    let sha = std::env::var("RPI_CODEMODE_NATIVE_SHA256").expect("explicit native SHA256");
    assert_eq!(digest(&bin), sha);
    let source = absolute("RPI_CODEMODE_NATIVE_EVIDENCE");
    let out = absolute("RPI_CODEMODE_HTTP_EVIDENCE_OUT");
    fs::create_dir(&out).expect("output must be NEW, never reuse failed evidence");
    save(
        out.join("results.json"),
        &json!({"status":"RUNNING","hostSha256":sha,"source":source}),
    );
    let launcher = out.join(if cfg!(windows) {
        "native-launcher.exe"
    } else {
        "native-launcher"
    });
    let launcher_src = out.join("native-launcher.rs");
    fs::write(
        &launcher_src,
        include_str!("fixtures/tool_program_native_launcher.rs"),
    )
    .unwrap();
    assert!(std::process::Command::new("rustc")
        .args(["--edition", "2021"])
        .arg(&launcher_src)
        .arg("-o")
        .arg(&launcher)
        .status()
        .unwrap()
        .success());
    let mut sources = Vec::new();
    for entry in fs::read_dir(&source).unwrap() {
        let root = entry.unwrap().path().join("sessions");
        if root.is_dir() {
            primaries(&root, &mut sources);
        }
    }
    sources.sort();
    assert_eq!(
        sources.len(),
        26,
        "this proof expects all 26 retained smoke sessions"
    );
    let mut results = Vec::new();
    for (i, original) in sources.iter().enumerate() {
        let original_hash = digest(original);
        let entries = lines(original);
        let id = entries[0]["id"].as_str().unwrap().to_owned();
        uuid::Uuid::parse_str(&id).unwrap();
        // Genuine native primary is the oracle. Follow the saved main branch, not all history.
        let leaf = entries
            .iter()
            .rev()
            .find(|e| e["kind"] == "entry" && e["lane"] == "main")
            .map(|e| e["id"].as_str().unwrap().to_owned());
        let by_id: HashMap<_, _> = entries
            .iter()
            .filter(|e| e["kind"] == "entry")
            .map(|e| (e["id"].as_str().unwrap(), e))
            .collect();
        let mut branch = Vec::new();
        let mut next = leaf.as_deref();
        while let Some(key) = next {
            let e = by_id[key];
            branch.push(e);
            next = e["parentId"].as_str();
            assert!(branch.len() <= entries.len());
        }
        branch.reverse();
        let begins: Vec<_> = branch
            .iter()
            .filter(|e| e["customType"] == "rpi.tool_program.child_begin.v1")
            .collect();
        let mut phases = HashMap::<String, Vec<ChildPhase>>::new();
        for e in &branch {
            if e["customType"] == "rpi.tool_program.child_phase.v1" {
                let mut v = e["data"].clone();
                v["entryId"] = e["id"].clone();
                v["seq"] = e["seq"].clone();
                let p: ChildPhase = serde_json::from_value(v).unwrap();
                phases.entry(p.child_id.clone()).or_default().push(p);
            }
        }
        let case = out.join(format!("session-{i}"));
        let copied = case.join("sessions/copied");
        fs::create_dir_all(&copied).unwrap();
        let saved = copied.join(original.file_name().unwrap());
        fs::copy(original, &saved).unwrap();
        let progress = PathBuf::from(format!("{}.progress.jsonl", original.display()));
        let progress_hash = progress.exists().then(|| digest(&progress));
        if progress.exists() {
            fs::copy(
                &progress,
                PathBuf::from(format!("{}.progress.jsonl", saved.display())),
            )
            .unwrap();
        }
        let project = case.join("workspace");
        fs::create_dir(&project).unwrap();
        let registry = case.join("registry.json");
        Registry::load(registry.clone())
            .unwrap()
            .upsert(RegistryEntry {
                session_id: id.clone(),
                project_id: None,
                title: None,
                parent_session_id: None,
                fork_entry_id: None,
                relation: None,
                created_at: 0,
                updated_at: 0,
            })
            .unwrap();
        let core = Service::new(BackendConfig {
            project,
            registry,
            rpi_bin: launcher.clone(),
            session_dir: Some(case.join("sessions")),
            ..Default::default()
        })
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = rpi_httpcore::AppConfig {
            port: address.port(),
            allowed_hosts: Some(vec![address.to_string()]),
            ..Default::default()
        };
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let password = uuid::Uuid::new_v4().to_string();
        let previous = std::env::var_os("RPI_HTTP_PASSWORD");
        std::env::set_var("RPI_HTTP_PASSWORD", &password);
        let app = rpi_httpcore::router(core.clone(), &config, receiver.clone());
        match previous {
            Some(v) => std::env::set_var("RPI_HTTP_PASSWORD", v),
            None => std::env::remove_var("RPI_HTTP_PASSWORD"),
        }
        let server = tokio::spawn(async move {
            let mut receiver = receiver;
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.changed().await;
                })
                .await
                .unwrap();
        });
        // Catch test assertions so Service shutdown always precedes listener shutdown.
        let attempt = AssertUnwindSafe(async {
            let opened = service::open_session(&core, id.clone()).await;
            save(case.join("open.json"), &json!({"kind":format!("{:?}",opened.0),"value":opened.1}));
            assert_eq!(opened.0, ReplyKind::Success, "{:?}", opened.1);
            let host = core.live_session(&id).await.unwrap();
            save(case.join("native-ready.json"), &host.ready());
            let mut raw = host.subscribe_raw(); let mut events = host.subscribe();
            assert_eq!(digest(&saved), original_hash, "open must preserve primary");
            let base = format!("http://{address}");
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(40)).build().unwrap();
            let login = client.post(format!("{base}/api/v1/auth")).header("X-Rpi-Web","1").header("Origin",&base).json(&json!({"password":password})).send().await.unwrap();
            assert_eq!(login.status(), 200);
            let cookie = login.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned();
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("cookie", cookie.parse().unwrap()); headers.insert("x-rpi-web", "1".parse().unwrap()); headers.insert("origin", base.parse().unwrap());
            drop(client);
            let mut http = Http { client:reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(40)).default_headers(headers).build().unwrap(), base, dir:case.clone(), calls:0, max_response:0 };
            let snap: ChildSnapshot = http.post(&id,"snapshot",&fresh(),200).await;
            assert!(!snap.resync_required && snap.events.is_empty());
            assert!(serde_json::to_vec(&snap).unwrap().len() <= 131072);
            assert_eq!(snap.page.leaf_id, leaf);
            let expected_ids: Vec<_> = begins.iter().rev().take(8).map(|e| e["id"].as_str().unwrap()).collect();
            assert_eq!(snap.page.children.iter().map(|c| c.child_id.as_str()).collect::<Vec<_>>(), expected_ids);
            for o in &snap.observations {
                let p = &phases[&o.child_id];
                let f = p.iter().find(|p| p.phase == ChildPhaseKind::Final);
                let c = p.iter().find(|p| p.phase == ChildPhaseKind::Closed);
                assert_eq!(o.final_phase.as_ref(), f); assert_eq!(o.closed_phase.as_ref(), c);
                let outcome = match (f,c) {
                    (Some(f), Some(c)) if c.index == f.index+1 && c.seq > f.seq => {
                        if c.interrupted == Some(true) { "interrupted" } else if c.failed == Some(true) || f.is_error == Some(true) { "failed" } else if f.cancelled == Some(true) { "cancelled" } else { "succeeded" }
                    }, (Some(_),_) => "finalObserved", _ => "unknown"
                };
                assert_eq!(serde_json::to_value(o.outcome).unwrap(), outcome);
            }
            let mut q = ChildPageQuery { leaf_id:leaf.clone(), cursor:None, direction:Direction::NewestFirst, limit:1, max_bytes:32768 };
            let mut ids = Vec::new(); let mut child_continuations = 0;
            for _ in 0..256 {
                let page: ChildPage = http.post(&id,"children/query",&q,200).await;
                assert_eq!(page.leaf_id, leaf); assert!(page.children.len() <= 1);
                assert!(serde_json::to_vec(&page).unwrap().len() <= q.max_bytes);
                for c in page.children { let b = begins.iter().find(|b| b["id"] == c.child_id).unwrap();
                    assert_eq!(c.seq, b["seq"].as_u64().unwrap());
                    let v = serde_json::to_value(&c).unwrap();
                    for (key,value) in b["data"].as_object().unwrap() { assert_eq!(&v[key], value); }
                    ids.push(c.child_id);
                }
                q.cursor = page.next_cursor; if q.cursor.is_none() { break; } child_continuations += 1;
            }
            assert!(q.cursor.is_none());
            assert_eq!(ids.iter().map(String::as_str).collect::<Vec<_>>(), begins.iter().rev().map(|e| e["id"].as_str().unwrap()).collect::<Vec<_>>());
            let mut phase_continuations = 0; let mut body_continuations = 0; let mut chunks = 0; let mut max_body_bytes = 0;
            for child in &snap.page.children {
                let mut q = ChildPhaseQuery { child_id:child.child_id.clone(), leaf_id:leaf.clone(), cursor:None, direction:Direction::NewestFirst, limit:1, max_bytes:32768 };
                let mut actual = Vec::new();
                for _ in 0..256 {
                    let page: ChildPhasePage = http.post(&id,"phases/query",&q,200).await;
                    assert_eq!(Some(&page.leaf_id),leaf.as_ref()); assert!(page.phases.len() <= 1 && page.scanned <= 128);
                    assert!(serde_json::to_vec(&page).unwrap().len() <= q.max_bytes);
                    actual.extend(page.phases); q.cursor = page.next_cursor;
                    if q.cursor.is_none() { break; } phase_continuations += 1;
                }
                assert!(q.cursor.is_none());
                assert_eq!(actual, phases[&child.child_id].iter().rev().cloned().collect::<Vec<_>>());
                // Hydrate one genuine descriptor per selected child, with bounded pages.
                if let Some(p) = actual.iter().find(|p| p.descriptor.is_some()) {
                    let d = p.descriptor.as_ref().unwrap();
                    let mut q = ChildBodyQuery { child_id:child.child_id.clone(), phase_entry_id:p.entry_id.clone(), descriptor_id:d.descriptor_id.clone(), leaf_id:leaf.clone(), cursor:None, limit:1, max_bytes:65536 };
                    let mut whole = Sha256::new(); let mut body_bytes = 0;
                    for _ in 0..1024 {
                        let page: ChildBodyPage = http.post(&id,"body/query",&q,200).await;
                        assert_eq!(&page.body.descriptor,d); assert!(page.body.chunks.len() <= 1 && page.body.byte_length <= 65536);
                        max_body_bytes = max_body_bytes.max(page.body.byte_length);
                        for c in page.body.chunks {
                            let native = branch.iter().find(|e| e["customType"] == "rpi.tool_program.body_chunk.v1" && e["data"]["bodyId"] == d.body_id && e["data"]["index"] == c.index).unwrap();
                            assert_eq!(native["data"]["data"],c.data); assert_eq!(native["data"]["sha256"],c.sha256);
                            let bytes = decode(&c.data); assert_eq!(bytes.len(),c.byte_length);
                            assert_eq!(format!("{:x}",Sha256::digest(&bytes)),c.sha256);
                            whole.update(&bytes); body_bytes += bytes.len(); chunks += 1;
                        }
                        q.cursor = page.next_cursor; if q.cursor.is_none() { break; } body_continuations += 1;
                    }
                    assert!(q.cursor.is_none()); assert_eq!(body_bytes,d.byte_length); assert_eq!(format!("{:x}",whole.finalize()),d.sha256);
                }
            }
            // Actual live host rejection: a syntactically valid but absent native leaf.
            let bad_leaf = ChildPageQuery { leaf_id:Some(uuid::Uuid::new_v4().to_string()), cursor:None, direction:Direction::NewestFirst, limit:1,max_bytes:32768 };
            // Frozen native emits a text-only SessionError, so core deliberately
            // sanitizes this to 502, NOT a claimed structured 409 conflict.
            let rejection: Value = http.post(&id,"children/query",&bad_leaf,502).await;
            assert_eq!(rejection,json!({"error":"native child query rejected"}));
            let mut bad_client = fresh(); bad_client.children.limit = 9;
            let _: Value = http.post(&id,"snapshot",&bad_client,400).await;
            let mut bad_page = fresh().children; bad_page.limit = 33;
            let _: Value = http.post(&id,"children/query",&bad_page,400).await;
            // Optional parent-owned route: read initial snapshot only, never await execution.
            let mut response = http.client.get(format!("{}/api/v1/sessions/{id}/tool-program/events",http.base)).send().await.unwrap();
            let sse = if response.status() == 404 { "SKIPPED_ROUTE_ABSENT" } else {
                assert_eq!(response.status(),200);
                let mut bytes = Vec::new();
                loop { let chunk = response.chunk().await.unwrap().expect("initial SSE frame"); bytes.extend(chunk); assert!(bytes.len() <= 262144);
                    if bytes.windows(2).any(|w| w == b"\n\n") { break; }
                }
                fs::write(case.join("sse-initial.txt"),&bytes).unwrap();
                let text = String::from_utf8(bytes).unwrap();
                let data = text.lines().find_map(|l| l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:"))).expect("SSE data");
                let initial: Value = serde_json::from_str(data).unwrap();
                let initial = initial.get("snapshot").unwrap_or(&initial);
                let initial: ChildSnapshot = serde_json::from_value(initial.clone()).unwrap();
                assert_eq!(serde_json::to_value(&initial.page).unwrap(),serde_json::to_value(&snap.page).unwrap());
                assert_eq!(serde_json::to_value(&initial.observations).unwrap(),serde_json::to_value(&snap.observations).unwrap());
                "INITIAL_SNAPSHOT_ONLY"
            };
            drop(response); drop(http.client);
            let mut raw_count = 0; let mut event_count = 0;
            loop { match raw.try_recv() {
                Ok(e) => { assert!(!execution(&e)); raw_count += 1; },
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(e) => panic!("raw audit lost frames: {e}"),
            }}
            loop { match events.try_recv() {
                Ok(e) => { assert!(!execution(&e.value)); event_count += 1; },
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(e) => panic!("event audit lost frames: {e}"),
            }}
            assert_eq!(digest(&saved),original_hash); assert_eq!(digest(original),original_hash);
            json!({"sessionId":id,"source":original,"sourceSha256":original_hash,"selectedChildren":snap.page.children.len(),"allChildren":ids.len(),"childContinuations":child_continuations,"phaseContinuations":phase_continuations,"bodyContinuations":body_continuations,"verifiedBodyChunks":chunks,"maxDecodedBodyPageBytes":max_body_bytes,"snapshotBytes":serde_json::to_vec(&snap).unwrap().len(),"httpCalls":http.calls,"maxResponseBytes":http.max_response,"rawFramesObserved":raw_count,"eventsObserved":event_count,"sse":sse})
        }).catch_unwind().await;
        core.shutdown().await;
        shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
        let native_frames = case.join("native-output.jsonl");
        let frames = if native_frames.exists() {
            lines(&native_frames)
        } else {
            Vec::new()
        };
        let input = case.join("native-input.jsonl");
        let commands = if input.exists() {
            lines(&input)
        } else {
            Vec::new()
        };
        let execution_frames = frames.iter().filter(|e| execution(e)).count();
        let execution_commands = commands
            .iter()
            .filter(|e| {
                !matches!(
                    e["type"].as_str(),
                    Some(
                        "get_state"
                            | "get_session_branch"
                            | "get_durable_state"
                            | "get_tool_program_children"
                            | "get_tool_program_child_phases"
                            | "get_tool_program_child_body"
                            | "stop"
                    )
                )
            })
            .count();
        save(
            case.join("cleanup.json"),
            &json!({"serviceShutdown":true,"listenerJoined":true,"executionFrames":execution_frames,"nonReadOnlyCommands":execution_commands,"nativeFrames":frames.len(),"commands":commands.len()}),
        );
        if let Err(panic) = attempt {
            save(
                out.join("results.json"),
                &json!({"status":"FAILED","failedSession":i,"completed":results,"cleanup":"service shutdown then listener joined"}),
            );
            std::panic::resume_unwind(panic);
        }
        assert_eq!(execution_frames, 0);
        assert_eq!(execution_commands, 0, "inspect native-input.jsonl");
        assert_eq!(digest(original), original_hash);
        assert_eq!(digest(&saved), original_hash);
        if let Some(hash) = progress_hash {
            assert_eq!(digest(&progress), hash);
        }
        results.push(attempt.unwrap());
        save(
            out.join("results.json"),
            &json!({"status":"RUNNING","results":results}),
        );
    }
    assert_eq!(digest(&bin), sha);
    let total = |key: &str| {
        results
            .iter()
            .map(|r| r[key].as_u64().unwrap())
            .sum::<u64>()
    };
    assert!(
        total("childContinuations") > 0
            && total("phaseContinuations") > 0
            && total("bodyContinuations") > 0
            && total("verifiedBodyChunks") > 0
    );
    let result = json!({"status":"PASSED","sessions":results.len(),"selectedChildren":total("selectedChildren"),"allChildren":total("allChildren"),"httpCalls":total("httpCalls"),"verifiedBodyChunks":total("verifiedBodyChunks"),"childContinuations":total("childContinuations"),"phaseContinuations":total("phaseContinuations"),"bodyContinuations":total("bodyContinuations"),"executionFrames":0,"nonReadOnlyCommands":0,"hostSha256":sha,"launcherSha256":digest(&launcher),"testSourceSha256":format!("{:x}",Sha256::digest(include_bytes!("tool_program_native.rs"))),"scope":"Windows frozen-native read-only recovered HTTP child queries; optional initial SSE only, not live execution overlap/full parity/UI/CI/Unix/deployment","results":results});
    save(out.join("results.json"), &result);
    println!("{result}");
}
