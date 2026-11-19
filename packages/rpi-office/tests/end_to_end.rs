//! End-to-end: a pane drives an `office_*` tool through the real tool drive,
//! and a pane-typed prompt streams deltas + a final reply back to that pane.
//!
//! This is the path the add-in actually uses, exercised through the registered
//! ABI callbacks (`execute`/`poll`) and the host event handlers.

mod support;

use futures_util::{SinkExt, StreamExt};
use rpi_plugin_sdk::{EventTag, StbString, StbStringRef, StepResultTag};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16) -> Ws {
    let (ws, _) = connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("websocket connect");
    ws
}

async fn send_frame(ws: &mut Ws, value: Value) {
    ws.send(Message::text(value.to_string()))
        .await
        .expect("send frame");
}

async fn recv_frame(ws: &mut Ws) -> Value {
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for a server frame")
        .expect("stream ended")
        .expect("websocket error");
    match next {
        Message::Text(text) => serde_json::from_str(&text).expect("server frame is JSON"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

fn hello() -> Value {
    json!({
        "type": "hello",
        "protocolVersion": 3,
        "host": "excel",
        "clientName": "pi-for-office-e2e",
        "paneId": "pane-e2e",
        "document": {"id": "id-1", "name": "Book1.xlsx", "source": "document.url"},
        "ops": ["excel.read_range", "excel.get_overview"],
        "catalogVersion": 3,
    })
}

/// Run one tool call through the registered `execute`/`poll` callbacks.
fn run_tool(name: &str, params: Value) -> Result<Value, String> {
    let (_, _, callbacks) = support::tool(name).unwrap_or_else(|| panic!("{name} is registered"));
    let handle = (callbacks.execute)(
        StbStringRef::from_str("call-1"),
        StbString::from_string(params.to_string()),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    assert!(!handle.is_null(), "{name} returned a null handle");
    let mut polls = 0;
    let result = loop {
        let result = (callbacks.poll)(handle, None, std::ptr::null_mut());
        match result.tag {
            StepResultTag::Pending => {
                polls += 1;
                assert!(polls < 2000, "{name} never finished");
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => break result,
        }
    };
    let (callbacks_destroy, payload) = (callbacks.destroy, result);
    callbacks_destroy(handle);
    match payload.tag {
        StepResultTag::Done => {
            // SAFETY: the tag selects the live union arm; the plugin produced
            // the string, so the host frees it through the SDK callback.
            let stb = unsafe { payload.done_payload().result };
            // SAFETY: the plugin produced this string; the host frees it once.
            let text = unsafe { stb.to_string_lossy() };
            unsafe { rpi_plugin_sdk::callbacks::free_string(stb) };
            serde_json::from_str(&text).map_err(|error| format!("invalid result JSON: {error}"))
        }
        StepResultTag::Err => {
            // SAFETY: as above, for the Err arm.
            let stb = unsafe { payload.err_payload().message };
            // SAFETY: as above, for the Err arm.
            let text = unsafe { stb.to_string_lossy() };
            unsafe { rpi_plugin_sdk::callbacks::free_string(stb) };
            Err(text)
        }
        StepResultTag::Pending => unreachable!(),
    }
}

#[tokio::test]
async fn pane_drives_a_tool_and_receives_streamed_replies() {
    support::register_plugin();
    // A print-mode session never dispatches SessionStart, so AgentStart must be
    // enough to resolve flags and reconcile the active tool set.
    support::run_empty_event(EventTag::AgentStart);

    let port = support::free_port();
    support::run_command("office", &format!("connect {port}"));

    let mut ws = connect(port).await;
    send_frame(&mut ws, hello()).await;
    let welcome = recv_frame(&mut ws).await;
    assert_eq!(welcome["type"], json!("welcome"));

    support::wait_until("the pane to attach", || {
        support::health(port)["panes"].as_array().unwrap().len() == 1
    });
    // Attaching a pane activates exactly the ops it advertises.
    support::wait_until("the advertised tools to activate", || {
        let active = support::active_tools();
        active.contains(&"office_excel_read_range".to_string())
            && active.contains(&"office_list_documents".to_string())
            && !active.contains(&"office_excel_charts".to_string())
    });

    // ── tool proxy: execute → pane → tool_result → poll ────────────────
    let (_, _, callbacks) = support::tool("office_excel_read_range").expect("registered");
    let handle = (callbacks.execute)(
        StbStringRef::from_str("call-1"),
        StbString::from_string(
            json!({"range": "A1:B2", "document": "id-1", "__rpi": {"cwd": "/tmp", "sessionId": "s"}})
                .to_string(),
        ),
        Some(rpi_plugin_sdk::callbacks::free_string),
    );
    let call = recv_frame(&mut ws).await;
    assert_eq!(call["type"], json!("tool_call"));
    assert_eq!(call["tool"], json!("excel.read_range"));
    assert_eq!(call["documentId"], json!("id-1"));
    assert!(call["args"].get("document").is_none());
    assert!(
        call["args"].get("__rpi").is_none(),
        "host metadata must never reach the pane"
    );
    assert_eq!(call["args"]["range"], json!("A1:B2"));

    let call_id = call["id"].as_str().unwrap().to_string();
    send_frame(
        &mut ws,
        json!({"type": "tool_result", "id": call_id, "ok": true, "text": "A1=1 B1=2", "details": {"cells": 4}}),
    )
    .await;

    let mut polls = 0;
    let result = loop {
        let result = (callbacks.poll)(handle, None, std::ptr::null_mut());
        if result.tag != StepResultTag::Pending {
            break result;
        }
        polls += 1;
        assert!(polls < 2000, "the tool never finished");
        std::thread::sleep(Duration::from_millis(5));
    };
    (callbacks.destroy)(handle);
    assert_eq!(result.tag, StepResultTag::Done);
    // SAFETY: Done selects the live union arm; the host frees the string once.
    let stb = unsafe { result.done_payload().result };
    // SAFETY: Done selects the live union arm; the host frees the string once.
    let text = unsafe { stb.to_string_lossy() };
    unsafe { rpi_plugin_sdk::callbacks::free_string(stb) };
    let value: Value = serde_json::from_str(&text).expect("tool result JSON");
    assert_eq!(value["content"][0]["type"], json!("text"));
    assert_eq!(value["content"][0]["text"], json!("A1=1 B1=2"));
    assert_eq!(value["details"]["cells"], json!(4));

    // The discovery tool answers from the registry, not from the pane.
    let listing = run_tool("office_list_documents", json!({"host": "excel"})).expect("listing");
    let text = listing["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Book1.xlsx"), "{text}");
    assert!(text.contains("id-1"), "{text}");
    assert!(text.contains("| document | id | host | focused | windows |"));

    // ── pane prompt → host, tagged with the document ────────────────────
    send_frame(
        &mut ws,
        json!({"type": "user_message", "text": "  sum A1:A3 "}),
    )
    .await;
    support::wait_until("the pane prompt to reach the host", || {
        !support::sent_messages().is_empty()
    });
    let sent = support::sent_messages();
    assert_eq!(sent[0], "[office:excel \"Book1.xlsx\" id=id-1]\nsum A1:A3");

    // ── delivered turn streams deltas, then the final reply ─────────────
    support::run_message_event(
        EventTag::MessageStart,
        json!({"role": "user", "content": [{"type": "text", "text": sent[0]}]}),
    );
    support::run_message_event(
        EventTag::MessageUpdate,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "部分"}}),
    );
    let delta = recv_frame(&mut ws).await;
    assert_eq!(delta["type"], json!("agent_message"));
    assert_eq!(delta["kind"], json!("delta"));
    assert_eq!(delta["text"], json!("部分"));
    let prompt_id = delta["promptId"].as_str().expect("prompt id").to_string();
    assert!(prompt_id.starts_with("prompt-"));

    support::run_message_event(
        EventTag::MessageEnd,
        json!({
            "role": "assistant",
            "id": "msg-1",
            "content": [{"type": "text", "text": "部分完成"}]
        }),
    );
    let final_message = recv_frame(&mut ws).await;
    assert_eq!(final_message["kind"], json!("final"));
    assert_eq!(final_message["text"], json!("部分完成"));
    assert_eq!(final_message["messageId"], json!("msg-1"));
    assert_eq!(final_message["promptId"], json!(prompt_id));

    // ── a TUI-initiated turn never reaches the pane ────────────────────
    support::run_message_event(
        EventTag::MessageStart,
        json!({"role": "user", "content": [{"type": "text", "text": "a terminal question"}]}),
    );
    support::run_message_event(
        EventTag::MessageUpdate,
        json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "terminal"}}),
    );
    support::run_message_event(
        EventTag::MessageEnd,
        json!({"role": "assistant", "content": [{"type": "text", "text": "terminal answer"}]}),
    );
    let quiet = tokio::time::timeout(Duration::from_millis(400), ws.next()).await;
    assert!(
        quiet.is_err(),
        "a terminal turn must not be forwarded to the pane: {quiet:?}"
    );

    // ── pane Stop aborts the run through the host action ────────────────
    send_frame(
        &mut ws,
        json!({"type": "user_message", "text": "stop this"}),
    )
    .await;
    support::wait_until("the second pane prompt", || {
        support::sent_messages().len() >= 2
    });
    let second_prompt = support::sent_messages()[1].clone();
    support::run_message_event(
        EventTag::MessageStart,
        json!({"role": "user", "content": [{"type": "text", "text": second_prompt}]}),
    );
    send_frame(&mut ws, json!({"type": "abort"})).await;
    support::wait_until("the host Abort action", || support::abort_count() >= 1);
    // The aborted turn must not leak a late final into the stopped pane.
    support::run_message_event(
        EventTag::MessageEnd,
        json!({"role": "assistant", "content": [{"type": "text", "text": "late"}]}),
    );
    let quiet = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        quiet.is_err(),
        "an aborted turn must not send a final to the pane: {quiet:?}"
    );

    // ── pane detach deactivates its tools again ─────────────────────────
    drop(ws);
    support::wait_until("the pane to detach", || {
        support::health(port)["panes"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    support::wait_until("the office tools to deactivate", || {
        !support::active_tools()
            .iter()
            .any(|name| name == "office_excel_read_range")
    });
    // The bridge stays on and keeps the discovery tool available.
    assert!(support::active_tools().contains(&"office_list_documents".to_string()));

    support::run_command("office", "close");

    // ── actionable errors when nothing can answer ───────────────────────
    // Bridge off: the tool reports the offline hint instead of hanging.
    let error = run_tool("office_excel_read_range", json!({"range": "A1"})).expect_err("offline");
    assert!(error.contains("/office connect"), "{error}");

    let port = support::free_port();
    support::run_command("office", &format!("connect {port}"));
    // Bridge on, no pane of that host: the error names the add-in.
    let error = run_tool("office_excel_read_range", json!({"range": "A1"})).expect_err("no pane");
    assert!(
        error.contains("no excel workbook/document is attached"),
        "{error}"
    );

    support::run_command("office", "close");

    // ── reload safety: begin/finish shutdown join both plugin threads ───
    let port = support::free_port();
    support::run_command("office", &format!("connect {port}"));
    assert_eq!(support::health(port)["ok"], json!(true));
    support::run_begin_shutdown();
    support::wait_until("the bridge to stop listening", || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err()
    });
    assert_eq!(support::run_finish_shutdown(), 0);
    assert_eq!(
        support::run_finish_shutdown(),
        0,
        "finish_shutdown is idempotent"
    );
    // Everything was released: a reloaded session can claim the port again.
    support::run_command("office", &format!("connect {port}"));
    assert_eq!(support::health(port)["ok"], json!(true));
    support::run_command("office", "close");
}
