use super::*;
use crate::host::TestEvents;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, Lines};

fn state(tokens: Value, window: Value) -> Value {
    json!({"sessionId":"native-session", "context":{"tokens":tokens,"window":window}})
}

#[test]
fn unknown_context_and_zero_window_are_not_zero_usage() {
    for value in [
        json!({}),
        json!({"context":null}),
        json!({"context":{"tokens":7,"window":0}}),
    ] {
        assert_eq!(parse_context_usage(&value).unwrap(), None);
    }
    // Unknown usage does not require identity.
    assert_eq!(
        parse_context_usage(&json!({"sessionId":false})).unwrap(),
        None
    );
}

#[test]
fn zero_over_window_and_u64_bounds_are_preserved() {
    for (tokens, window) in [(0, 100), (200, 100), (u64::MAX, u64::MAX)] {
        let usage = parse_context_usage(&state(json!(tokens), json!(window)))
            .unwrap()
            .unwrap();
        assert_eq!(
            usage,
            ContextUsage {
                session_id: "native-session".into(),
                used_tokens: tokens,
                window_tokens: window,
                estimated: None,
                trailing_tokens: None,
            }
        );
    }
}

#[test]
fn malformed_state_context_and_required_numbers_fail() {
    for value in [Value::Null, json!([]), json!(true), json!("state")] {
        assert!(parse_context_usage(&value).is_err());
    }
    for context in [
        json!(false),
        json!(0),
        json!("context"),
        json!([]),
        json!({}),
    ] {
        assert!(parse_context_usage(&json!({"context":context})).is_err());
    }
    for field in ["tokens", "window"] {
        for invalid in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!("1"),
            json!(false),
            json!([]),
            json!({}),
            json!(18446744073709551616.0),
        ] {
            let mut value = state(json!(1), json!(100));
            value["context"][field] = invalid;
            assert!(parse_context_usage(&value).is_err(), "{value}");
        }
        let mut value = state(json!(1), json!(100));
        value["context"].as_object_mut().unwrap().remove(field);
        assert!(parse_context_usage(&value).is_err());
    }
}

#[test]
fn optional_fields_are_strict_and_null_is_none() {
    let mut value = state(json!(1), json!(100));
    value["context"]["estimated"] = Value::Null;
    value["context"]["trailingTokens"] = Value::Null;
    let usage = parse_context_usage(&value).unwrap().unwrap();
    assert_eq!(usage.estimated, None);
    assert_eq!(usage.trailing_tokens, None);
    for estimated in [true, false] {
        value["context"]["estimated"] = json!(estimated);
        value["context"]["trailingTokens"] = json!(u64::MAX);
        let usage = parse_context_usage(&value).unwrap().unwrap();
        assert_eq!(usage.estimated, Some(estimated));
        assert_eq!(usage.trailing_tokens, Some(u64::MAX));
    }
    for invalid in [json!(0), json!("true"), json!([]), json!({})] {
        value["context"]["estimated"] = invalid;
        assert!(parse_context_usage(&value).is_err());
    }
    value["context"]["estimated"] = json!(true);
    for invalid in [
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(false),
        json!([]),
        json!({}),
    ] {
        value["context"]["trailingTokens"] = invalid;
        assert!(parse_context_usage(&value).is_err());
    }
    // A zero window does not hide incorrectly typed optional fields.
    value["context"]["window"] = json!(0);
    assert!(parse_context_usage(&value).is_err());
}

#[test]
fn known_context_requires_nonempty_byte_bounded_identity() {
    let mut value = state(json!(1), json!(100));
    value.as_object_mut().unwrap().remove("sessionId");
    assert!(parse_context_usage(&value).is_err());
    for invalid in [
        Value::Null,
        json!(false),
        json!(123),
        json!(""),
        json!("x".repeat(4097)),
        json!("é".repeat(2049)),
    ] {
        value["sessionId"] = invalid;
        assert!(parse_context_usage(&value).is_err());
    }
    for valid in ["x".repeat(4096), "é".repeat(2048)] {
        value["sessionId"] = json!(valid);
        assert_eq!(
            parse_context_usage(&value).unwrap().unwrap().session_id,
            valid
        );
    }
}

struct Fixture {
    host: Arc<HostSession>,
    events: TestEvents,
    lines: Lines<BufReader<DuplexStream>>,
}
impl Fixture {
    fn new(advertised: bool) -> Self {
        let (writer, reader) = tokio::io::duplex(65536);
        let commands = if advertised {
            vec!["get_state"]
        } else {
            vec![]
        };
        let (host, events) = HostSession::test_connection_with_ready(
            writer,
            32,
            json!({"type":"ready", "protocol":"rpi", "supportedCommands":commands}),
        );
        Self {
            host,
            events,
            lines: BufReader::new(reader).lines(),
        }
    }
    fn read(&self) -> tokio::task::JoinHandle<Result<Option<ContextUsage>, String>> {
        let host = self.host.clone();
        tokio::spawn(async move { host.context_usage().await })
    }
    async fn command(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(1), self.lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["type"], "get_state");
        assert_eq!(request.as_object().unwrap().len(), 2); // type and correlation ID only
        request
    }
    fn reply(&self, request: &Value, data: Value) {
        // Late replies intentionally have no pending receiver.
        let _ = self
            .events
            .send(json!({"type":"response", "id":request["id"], "success":true, "data":data}));
    }
    async fn no_command(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(10), self.lines.next_line())
                .await
                .is_err()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn unsupported_does_not_send_a_command() {
    let mut fixture = Fixture::new(false);
    assert_eq!(fixture.host.context_usage().await.unwrap(), None);
    fixture.no_command().await;
}

#[tokio::test]
async fn advertised_rejection_is_a_fixed_error_not_unsupported() {
    let mut fixture = Fixture::new(true);
    let read = fixture.read();
    let request = fixture.command().await;
    fixture.events.send(json!({"type":"response", "id":request["id"], "success":false, "error":"arbitrary native detail"})).unwrap();
    assert_eq!(
        read.await.unwrap().unwrap_err(),
        "context usage: read failed"
    );
}

#[tokio::test]
async fn valid_data_is_parsed_and_bad_advertised_data_is_an_error() {
    let mut fixture = Fixture::new(true);
    for data in [
        state(json!(0), json!(100)),
        json!({"context":null}),
        state(json!("bad"), json!(100)),
    ] {
        let read = fixture.read();
        let request = fixture.command().await;
        fixture.reply(&request, data.clone());
        assert_eq!(read.await.unwrap(), parse_context_usage(&data));
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_releases_admission_and_late_reply_cannot_settle_next_read() {
    let mut fixture = Fixture::new(true);
    let started = tokio::time::Instant::now();
    let read = fixture.read();
    let old = fixture.command().await;
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(read.await.unwrap().is_err());
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    let mut next = fixture.read();
    let request = fixture.command().await;
    assert_ne!(old["id"], request["id"]);
    fixture.reply(&old, state(json!(999), json!(100)));
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut next)
        .await
        .is_err());
    fixture.reply(&request, state(json!(7), json!(100)));
    assert_eq!(next.await.unwrap().unwrap().unwrap().used_tokens, 7);
}

#[tokio::test(start_paused = true)]
async fn blocked_native_writer_is_inside_the_total_deadline() {
    let (writer, _unread_peer) = tokio::io::duplex(1);
    let (host, _events) = HostSession::test_connection_with_ready(
        writer,
        16,
        json!({"type":"ready", "protocol":"rpi", "supportedCommands":["get_state"]}),
    );
    let started = tokio::time::Instant::now();
    assert!(host.context_usage().await.is_err());
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    // Admission was released even though the native writer is still blocked.
    assert!(host.usage_gate.try_lock().is_ok());
    assert!(host.try_run_lease().is_ok());
}

#[tokio::test(start_paused = true)]
async fn single_flight_is_immediate_and_independent_of_run_lease() {
    let mut fixture = Fixture::new(true);
    let lease = fixture.host.try_run_lease().unwrap();
    let read = fixture.read();
    let request = fixture.command().await;
    let started = tokio::time::Instant::now();
    assert_eq!(
        fixture.host.context_usage().await.unwrap_err(),
        "context usage: busy"
    );
    assert_eq!(started.elapsed(), Duration::ZERO);
    fixture.no_command().await;
    drop(lease);
    let _lease = fixture.host.try_run_lease().unwrap();
    fixture.reply(&request, state(json!(1), json!(100)));
    assert!(read.await.unwrap().unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn dropping_caller_releases_admission_without_closing_host() {
    let mut fixture = Fixture::new(true);
    let read = fixture.read();
    let old = fixture.command().await;
    read.abort();
    assert!(read.await.unwrap_err().is_cancelled());
    let mut next = fixture.read();
    let request = fixture.command().await;
    fixture.reply(&old, state(json!(999), json!(100)));
    assert!(tokio::time::timeout(Duration::from_millis(1), &mut next)
        .await
        .is_err());
    fixture.reply(&request, state(json!(2), json!(100)));
    assert_eq!(next.await.unwrap().unwrap().unwrap().used_tokens, 2);
    assert!(!*fixture.host.closed_receiver().borrow());
}

#[tokio::test(start_paused = true)]
async fn closure_interrupts_pending_read_and_rejects_future_reads() {
    let mut fixture = Fixture::new(true);
    let read = fixture.read();
    let request = fixture.command().await;
    let started = tokio::time::Instant::now();
    fixture.host.terminate().await;
    assert_eq!(
        read.await.unwrap().unwrap_err(),
        "context usage: host closed"
    );
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(
        fixture.host.context_usage().await.unwrap_err(),
        "context usage: host closed"
    );
    fixture.reply(&request, state(json!(1), json!(100)));
}
