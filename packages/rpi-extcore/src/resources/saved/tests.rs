use super::*;
use serde_json::json;
fn view() -> Value {
    json!({"version":1,"type":"saved_session_read","success":true,"data":{"sessionId":"session","cwd":std::env::temp_dir(),"leafId":"message","source":"a".repeat(64),"messages":[{"entryId":"message","role":"user","content":"preserved"}],"cursor":null,"filtered":true,"customEntries":[],"binding":{"revision":0,"fingerprint":null,"operationId":null},"pending":{"executionQueueOrRecovery":false,"progress":false,"idleProofSupported":true}}})
}
#[test]
fn strict_saved_envelope_identity_cursor_and_limits() {
    let query = ResourceHistoryRequest::default();
    let good = view();
    let parse_value = |v: &Value| parse(true, &serde_json::to_vec(v).unwrap(), "session", &query);
    assert!(parse_value(&good).is_ok());
    for (pointer, value) in [
        ("/version", json!(2)),
        ("/type", json!("response")),
        ("/success", json!(false)),
        ("/data/sessionId", json!("other")),
        ("/data/filtered", json!(false)),
        ("/data/source", json!("not a digest")),
        ("/data/binding/revision", json!(1)),
        ("/data/messages/0/entryId", json!(null)),
        (
            "/data/customEntries",
            json!([{"type":"custom","customType":"rpi.value","data":{"secret":"never expose"}}]),
        ),
        (
            "/data/cursor",
            json!({"version":1,"source":"b".repeat(64),"leafId":"message","afterSeq":1}),
        ),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(parse_value(&bad).is_err(), "{pointer}");
    }
    let mut bad = good.clone();
    bad["data"]["unknown"] = json!(true);
    assert!(parse_value(&bad).is_err());
    let mut bad = good.clone();
    bad["data"]["executable"] = json!(true);
    assert!(parse_value(&bad).is_err());
    for limit in [0, 129, usize::MAX] {
        assert!(ResourceHistoryRequest {
            limit,
            cursor: None
        }
        .validate()
        .is_err());
    }
    let mut bad = good;
    bad["data"]["messages"] = json!(vec![json!({"entryId":"x"}); 129]);
    assert!(parse_value(&bad).is_err());
    for (code, kind) in [
        ("busy", ResourceErrorKind::Conflict),
        ("branch_changed", ResourceErrorKind::Conflict),
        ("unsupported", ResourceErrorKind::Unsupported),
        ("limit_exceeded", ResourceErrorKind::LimitExceeded),
        ("invalid_data", ResourceErrorKind::Unavailable),
    ] {
        let bytes=serde_json::to_vec(&json!({"version":1,"type":"saved_session_read","success":false,"error":{"code":code,"message":"native diagnostic"}})).unwrap();
        assert_eq!(
            parse(false, &bytes, "session", &query).unwrap_err().kind,
            kind
        );
    }
}
#[test]
fn fresh_native_and_durable_pending_never_authorize_offline_execution() {
    let parse_value = |v: &Value| {
        parse(
            true,
            &serde_json::to_vec(v).unwrap(),
            "session",
            &ResourceHistoryRequest::default(),
        )
        .unwrap()
    };
    let good = view();
    parse_value(&good).ensure_idle().unwrap();
    for field in ["executionQueueOrRecovery", "progress"] {
        let mut pending = good.clone();
        pending["data"]["pending"][field] = json!(true);
        assert_eq!(
            parse_value(&pending).ensure_idle().unwrap_err().kind,
            ResourceErrorKind::Conflict
        );
    }
    let mut legacy = good.clone();
    legacy["data"]["pending"]["idleProofSupported"] = json!(false);
    assert_eq!(
        parse_value(&legacy).ensure_idle().unwrap_err().kind,
        ResourceErrorKind::Unsupported
    );
    let mut pending = good.clone();
    pending["data"]["customEntries"] = json!([{"id":"intent-1","type":"custom","customType":crate::durable::ENTRY_TYPE,"data":{"format":rpi_durable_core::JOURNAL_FORMAT,"version":1,"kind":"snapshot","baseRevision":0,"revision":1,"state":{"revision":1,"queue":[{"id":"queue-1","text":"pending","status":"queued"}]}}}]);
    assert!(parse_value(&pending).ensure_idle().is_err());
    pending["data"]["customEntries"][0]["data"]["revision"] = json!(99);
    assert!(parse_value(&pending).ensure_idle().is_err());
}
