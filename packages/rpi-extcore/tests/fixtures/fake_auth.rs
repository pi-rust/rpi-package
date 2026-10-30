//! Deterministic native-auth peer: never reads or persists real credentials.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
fn send(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn hang() {
    std::thread::sleep(std::time::Duration::from_secs(120));
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let provider = &args[4];
    // Inherit pipes deliberately: killing only the parent must not pass tree tests.
    let _descendant = if provider == "tree" {
        Some(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["auth", "login", "--provider", "hang", "--rpc"])
                .spawn()
                .unwrap(),
        )
    } else {
        None
    };
    if let Ok(path) = std::env::var("AUTH_FIXTURE_PID") {
        let path = if provider == "tree" {
            format!("{path}.parent")
        } else {
            path
        };
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    if matches!(provider.as_str(), "hang" | "tree") {
        hang();
        return;
    }
    let descriptor = json!({"rpc":true,"provider":provider,"kind":"oauth"});
    if args[2] == "describe" {
        match provider.as_str() {
            "describe_oversize" => print!("{}", " ".repeat(65537)),
            "describe_wrong" => send(json!({"rpc":true,"provider":"other","kind":"oauth"})),
            "describe_no_rpc" => send(json!({"provider":provider,"kind":"oauth"})),
            "describe_exit" => {
                send(descriptor);
                std::process::exit(1);
            }
            _ => send(descriptor),
        }
        return;
    }
    match provider.as_str() {
        "oversize" => {
            print!("{}", "S".repeat(65537));
            return;
        }
        "malformed" => {
            eprintln!("SECRET_PASSWORD");
            println!("SECRET_PASSWORD");
            return;
        }
        "invalid_utf8" => {
            io::stdout().write_all(b"\xff\n").unwrap();
            return;
        }
        "unterminated" => {
            print!("{{\"type\":\"auth_ready\"}}");
            return;
        }
        _ => {}
    }
    // Matches native rpc.rs exactly: `rpc` is descriptor-only.
    let mut ready = json!({"type":"auth_ready","provider":provider,"kind":"oauth"});
    if provider == "wrong" {
        ready["provider"] = "SECRET_PASSWORD".into();
    }
    send(ready);
    if provider == "missing_result" {
        return;
    }
    if provider == "result_hang" {
        send(json!({"type":"auth_result","authenticated":true}));
        hang();
        return;
    }
    if provider == "pre_prompt" {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let mut interaction =
        json!({"type":"auth_interaction","requestId":"one","mode":"form","message":"fixture"});
    if provider == "empty_id" {
        interaction["requestId"] = "".into();
    }
    if provider == "url" {
        interaction["mode"] = "url".into();
        interaction["url"] = "https://example.invalid/SECRET".into();
        interaction["elicitationId"] = "SECRET".into();
        interaction["requestedSchema"] = json!({"type":"object","description":"SECRET"});
        interaction["message"] = "🔐 café SECRET".into();
    }
    if provider == "boundary" || provider == "over_boundary" {
        let base = serde_json::to_vec(&interaction).unwrap().len() + 1;
        let padding = 65536 - base + usize::from(provider == "over_boundary");
        interaction["message"] = format!("fixture{}", "x".repeat(padding)).into();
    }
    send(interaction.clone());
    if provider == "blocked_write" {
        hang();
        return;
    }
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["type"], "auth_response");
    assert_eq!(response["requestId"], "one");
    assert!(response.get("request_id").is_none());
    if provider == "duplicate_id" {
        send(interaction);
        hang();
        return;
    }
    let success = response["action"] == "accept";
    if provider == "invalid_result" {
        send(json!({"type":"auth_result","authenticated":"true"}));
        return;
    }
    send(json!({"type":"auth_result","authenticated":success,"error":"SECRET_PASSWORD"}));
    if provider == "late_exit" {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    std::process::exit(
        if provider == "bad_exit" || (!success && provider != "false_zero") {
            1
        } else {
            0
        },
    );
}
