// Standalone std-only mock native, compiled by resource_recovery.rs. No models/network.
use std::{
    io::{self, BufRead, Write},
    path::Path,
    time::Duration,
};
fn string(s: &str, key: &str) -> String {
    let Some((_, rest)) = s.split_once(&format!("\"{key}\":")) else {
        return String::new();
    };
    let rest = rest.trim_start();
    if !rest.starts_with('"') {
        return String::new();
    }
    let mut result = String::new();
    let mut escaped = false;
    for c in rest[1..].chars() {
        if escaped {
            result.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else {
            result.push(c);
        }
    }
    result
}
fn emit(s: String) {
    println!("{s}");
    io::stdout().flush().unwrap();
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |key: &str| args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone());
    if args.iter().any(|s| s == "--help") {
        emit("--resource-capabilities".into());
        return;
    }
    if args.iter().any(|s| s == "--resource-capabilities") {
        emit(r#"{"type":"resource_capabilities","version":1,"loadedFacts":false,"exactSelection":{"versions":[1],"startupFlag":"--resource-config","stateCommand":"get_resource_state","mode":"rpc","maxConfigBytes":65536,"maxSnapshotBytes":262144,"toolModes":["default","only","none"]},"transition":{"version":1,"startupFlag":"--resource-transition","maxRequestBytes":65536}}"#.into());
        return;
    }
    let fork = arg("--fork").is_some();
    let id = arg("--session-id").unwrap_or_else(|| format!("fork-{}", std::process::id()));
    let exact = arg("--resource-config");
    let selection = exact
        .as_ref()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|| "null".into());
    let mut model = arg("--model").unwrap_or_else(|| "baseline".into());
    let mut provider = arg("--provider").unwrap_or_else(|| "baseline-provider".into());
    let mut thinking = "off".to_string();
    let binding = format!(r#"{{"revision":1,"fingerprint":"{}"}}"#, "a".repeat(64));
    let generation = format!("host-{}", std::process::id());
    let resource = if exact.is_some() {
        format!(r#"{{"version":1,"enabled":true,"hostId":"{generation}","binding":{binding}}}"#)
    } else {
        r#"{"version":1,"enabled":false,"binding":{"revision":0}}"#.into()
    };
    emit(format!(
        r#"{{"type":"ready","protocol":"rpi","supportedCommands":["get_state","get_resource_state","get_messages","get_queue","get_session_branch","set_model","set_thinking_level","navigate","execute_command"],"resourceState":{resource}}}"#
    ));
    std::fs::write(format!("started-{id}"), std::process::id().to_string()).unwrap();
    let watched_id = id.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(20));
        if !Path::new("owner-alive").exists() {
            let _ = std::fs::write(format!("stopped-{watched_id}"), "fixture owner dropped");
            std::process::exit(0);
        }
    });
    let mut reads = 0;
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let kind = string(&line, "type");
        let request_id = string(&line, "id");
        if kind == "stop" {
            std::fs::write(format!("stopped-{id}"), "stopped").unwrap();
            return;
        }
        let mut success = true;
        let data = match kind.as_str() {
            "get_state" => format!(
                r#"{{"sessionId":"{id}","model":"{model}","provider":"{provider}","thinkingLevel":"{thinking}","activeOperation":null,"queue":{{"steeringCount":0,"followUpCount":0}}}}"#
            ),
            "get_resource_state" => {
                reads += 1;
                if fork && reads >= 2 && Path::new("pause-readback").exists() {
                    std::fs::write("readback-paused", &id).unwrap();
                    for _ in 0..1000 {
                        if !Path::new("pause-readback").exists() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                if fork && reads >= 2 && Path::new("fail-readback").exists() {
                    success = false;
                }
                format!(
                    r#"{{"version":1,"hostId":"{generation}","sessionId":"{id}","binding":{binding},"selection":{selection},"loadedSkills":[],"loadedExtensions":[],"registeredTools":[],"modelEffectiveTools":[]}}"#
                )
            }
            "get_messages" => r#"{"pendingResume":false,"messages":[]}"#.into(),
            "get_queue" => r#"{"count":0,"items":[]}"#.into(),
            "get_session_branch" => {
                if Path::new("pause-branch").exists() {
                    std::fs::write("branch-paused", &id).unwrap();
                    for _ in 0..1000 {
                        if !Path::new("pause-branch").exists() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                if Path::new("fail-metadata").exists() {
                    let _ = std::fs::create_dir("registry.json.tmp");
                }
                success = !Path::new("fail-branch").exists();
                r#"{"filtered":true,"entries":[]}"#.into()
            }
            "set_model" => {
                model = string(&line, "modelId");
                provider = string(&line, "provider");
                "{}".into()
            }
            "set_thinking_level" => {
                thinking = string(&line, "level");
                "{}".into()
            }
            "navigate" => {
                success = !Path::new("fail-navigation").exists();
                "{}".into()
            }
            _ => "{}".into(),
        };
        emit(format!(
            r#"{{"type":"response","id":"{request_id}","success":{success},"data":{data},"error":"injected {kind} failure"}}"#
        ));
    }
}
