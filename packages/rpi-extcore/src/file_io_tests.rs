use super::*;

fn id() -> String {
    format!("native:{}", uuid::Uuid::new_v4())
}
fn read() -> Value {
    json!({"type":"file_io_request", "requestId":id(), "operation":"read_text_file", "path":"/project/a", "maxBytes":MAX_TEXT_BYTES})
}
fn capabilities() -> FileIOCapabilities {
    FileIOCapabilities {
        read_text: true,
        write_text: true,
    }
}

#[test]
fn request_shapes_complete_range_paths_and_utf8_budgets() {
    let value = read();
    let request = parse_request(&value, capabilities()).unwrap();
    assert!(matches!(
        request.operation,
        FileIOOperation::ReadText {
            range: FileIOReadRange::Complete,
            ..
        }
    ));
    let mut lines = value.clone();
    lines["line"] = json!(1);
    lines["limit"] = json!(10);
    assert!(matches!(
        parse_request(&lines, capabilities()).unwrap().operation,
        FileIOOperation::ReadText {
            range: FileIOReadRange::Lines {
                offset_1based: 1,
                limit: 10
            },
            ..
        }
    ));
    for path in [
        "/tmp/a",
        "C:\\project\\a",
        "C:/project/a",
        "\\\\host\\share\\a",
    ] {
        let mut v = value.clone();
        v["path"] = json!(path);
        assert!(parse_request(&v, capabilities()).is_ok());
    }
    for path in ["", "relative", "C:relative", "a\0b", "\\relative"] {
        let mut v = value.clone();
        v["path"] = json!(path);
        assert!(parse_request(&v, capabilities()).is_err());
    }
    for (key, invalid) in [
        ("maxBytes", json!(MAX_TEXT_BYTES + 1)),
        ("maxBytes", json!(-1)),
        ("maxBytes", Value::Null),
        ("requestId", json!("native:not-uuid")),
        ("line", json!(1)),
        ("arbitrary", Value::Null),
    ] {
        let mut v = value.clone();
        v[key] = invalid;
        assert!(parse_request(&v, capabilities()).is_err());
    }
    lines["line"] = json!(0);
    assert!(parse_request(&lines, capabilities()).is_err());
    lines["line"] = json!(1);
    lines["limit"] = Value::Null;
    assert!(parse_request(&lines, capabilities()).is_err());
    assert!(parse_request(&value, FileIOCapabilities::default()).is_err());
    let mut write = json!({"type":"file_io_request","requestId":id(),"operation":"write_text_file","path":"/x","content":"é".repeat(MAX_TEXT_BYTES/2)});
    assert!(parse_request(&write, capabilities()).is_ok());
    write["content"] = json!("é".repeat(MAX_TEXT_BYTES / 2 + 1));
    assert!(parse_request(&write, capabilities()).is_err());
}

#[test]
fn typed_outcomes_emit_exactly_one_nonnull_result_or_bounded_known_error() {
    let request = parse_request(&read(), capabilities()).unwrap();
    let good = frame(
        &request,
        Ok(FileIOResponse::Text {
            content: "hello".into(),
        }),
    );
    assert_eq!(good.as_object().unwrap().len(), 3);
    assert_eq!(good["result"], json!({"content":"hello"}));
    assert!(good.get("error").is_none());
    for result in [
        Ok(FileIOResponse::Written),
        Ok(FileIOResponse::Text {
            content: "x".repeat(MAX_TEXT_BYTES + 1),
        }),
        Err(FileIOError::new(
            FileIOErrorCode::Denied,
            "x".repeat(MAX_ERROR_BYTES + 1),
        )),
    ] {
        let bad = frame(&request, result);
        assert!(bad.get("result").is_none());
        assert_eq!(bad["error"]["code"], "invalid_response");
    }
    for code in [
        FileIOErrorCode::Unsupported,
        FileIOErrorCode::Denied,
        FileIOErrorCode::NotFound,
        FileIOErrorCode::InvalidResponse,
        FileIOErrorCode::TooLarge,
        FileIOErrorCode::Timeout,
        FileIOErrorCode::Cancelled,
        FileIOErrorCode::Disconnected,
        FileIOErrorCode::InvalidPath,
    ] {
        let error = frame(&request, Err(FileIOError::new(code, "bounded")));
        assert!(error.get("result").is_none());
        assert_eq!(
            serde_json::from_value::<FileIOError>(error["error"].clone())
                .unwrap()
                .code,
            code
        );
    }
    let escaped = frame(
        &request,
        Ok(FileIOResponse::Text {
            content: "\0".repeat(MAX_TEXT_BYTES),
        }),
    );
    assert!(escaped.get("result").is_none());
    assert_eq!(escaped["error"]["code"], "too_large");
    let write = FileIORequest {
        request_id: id(),
        operation: FileIOOperation::WriteText {
            absolute_path: "/x".into(),
            content: "".into(),
        },
    };
    assert_eq!(
        frame(&write, Ok(FileIOResponse::Written))["result"],
        json!({})
    );
}

#[test]
fn encoded_payload_and_profile_limits_are_exact_not_a_global_frame_limit() {
    let value = json!("x".repeat(MAX_PAYLOAD_BYTES - 2));
    assert!(fits_payload(&value));
    assert!(!fits_payload(&json!("x".repeat(MAX_PAYLOAD_BYTES - 1))));
    let profile = json!({"version":1,"maxPending":64,"maxTextBytes":262144,"maxPayloadBytes":786432,"timeoutMs":60000});
    assert!(supports_profile(&profile));
    for key in profile.as_object().unwrap().keys() {
        let mut bad = profile.clone();
        bad[key] = Value::Null;
        assert!(!supports_profile(&bad));
    }
}

#[tokio::test]
async fn neutral_cancellation_is_sticky_and_multiconsumer() {
    let control = FileIOCancellation::default();
    let clone = control.clone();
    control.cancel();
    clone.cancelled().await;
    control.cancelled().await;
    assert!(clone.is_cancelled());
}
