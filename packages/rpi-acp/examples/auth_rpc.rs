//! Native auth wire fixture: no credentials, network, or persistent storage.
use serde_json::{json, Value};
use std::io::{BufRead, Write};
fn emit(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let provider = args
        .windows(2)
        .find(|a| a[0] == "--provider")
        .map(|a| a[1].as_str())
        .unwrap_or("anthropic");
    let mode = std::env::var("ACP_AUTH_FIXTURE").unwrap_or_else(|_| "key".into());
    let kind = if mode.starts_with("oauth") {
        "oauth"
    } else {
        "api_key"
    };
    if let Ok(path) = std::env::var("ACP_AUTH_PID_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    if args.iter().any(|a| a == "describe") {
        if mode == "unsupported" {
            std::process::exit(1);
        }
        if mode == "bad-descriptor" {
            println!("{{bad");
            return;
        }
        emit(json!({"rpc":true,"provider":provider,"kind":kind}));
        return;
    }
    if args.iter().any(|a| a == "check") {
        emit(json!({"ready":true,"provider":provider,"source":"fixture"}));
        return;
    }
    if args.iter().any(|a| a == "logout") {
        return;
    }
    emit(json!({"type":"auth_ready","provider":provider,"kind":kind}));
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    for (id, url) in if kind == "oauth" {
        vec![("browser", true), ("code", false)]
    } else {
        vec![("key", false)]
    } {
        let mut interaction = json!({"type":"auth_interaction","requestId":id,"mode":if url {"url"} else {"form"},"message":"Fixture login"});
        if url {
            interaction["url"] = "https://example.com/connect".into();
            interaction["elicitationId"] = "oauth-fixture".into();
        } else {
            interaction["requestedSchema"] = json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]});
        }
        emit(interaction);
        let response: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(response["type"], "auth_response");
        assert_eq!(response["requestId"], id);
        if response["action"] != "accept" {
            emit(json!({"type":"auth_result","authenticated":false}));
            std::process::exit(1);
        }
        if !url {
            assert!(response["content"]["answer"].is_string());
        }
    }
    if mode == "oauth-hang" {
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
    emit(json!({"type":"auth_result","authenticated":mode != "failed"}));
    if mode == "bad-exit" || mode == "failed" {
        std::process::exit(1);
    }
}
