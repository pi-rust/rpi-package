use super::*;
use crate::protocol::ClientRequestError;
use tokio::io::{AsyncBufReadExt, BufReader};

#[test]
fn filesystem_capability_matrix_is_independent_and_strict() {
    for (read, write) in [(false, false), (true, false), (false, true), (true, true)] {
        let caps = capabilities(
            &json!({"clientCapabilities":{"fs":{"readTextFile":read,"writeTextFile":write}}}),
        )
        .unwrap();
        assert_eq!(
            caps,
            FileIOCapabilities {
                read_text: read,
                write_text: write
            }
        );
    }
    assert_eq!(
        capabilities(&json!({})).unwrap(),
        FileIOCapabilities::default()
    );
    for fs in [
        Value::Null,
        json!(true),
        json!([]),
        json!({"readTextFile":1}),
        json!({"writeTextFile":{}}),
        json!({"readTextFile":null}),
    ] {
        assert!(capabilities(&json!({"clientCapabilities":{"fs":fs}})).is_err());
    }
}

fn read(range: FileIOReadRange) -> FileIORequest {
    FileIORequest {
        request_id: "native-id".into(),
        operation: FileIOOperation::ReadText {
            absolute_path: "/editor/unsaved.txt".into(),
            range,
            max_bytes: 262144,
        },
    }
}
fn write() -> FileIORequest {
    FileIORequest {
        request_id: "native-id".into(),
        operation: FileIOOperation::WriteText {
            absolute_path: "/editor/unsaved.txt".into(),
            content: "".into(),
        },
    }
}
fn handler() -> (
    Arc<Writer>,
    Arc<ClientFileIO>,
    tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
) {
    let (output, peer) = tokio::io::duplex(16384);
    let writer = Arc::new(Writer::with_output(output));
    // Public production negotiation is exercised by dispatcher tests too.
    let handler = Arc::new(ClientFileIO {
        capabilities: FileIOCapabilities {
            read_text: true,
            write_text: true,
        },
        writer: writer.clone(),
        session_id: "acp-session".into(),
    });
    (writer, handler, BufReader::new(peer).lines())
}
async fn line(lines: &mut tokio::io::Lines<BufReader<tokio::io::DuplexStream>>) -> Value {
    serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn exact_client_read_complete_and_lines_and_empty_content() {
    for range in [
        FileIOReadRange::Complete,
        FileIOReadRange::Lines {
            offset_1based: 3,
            limit: 7,
        },
    ] {
        let (writer, handler, mut lines) = handler();
        let ranged = matches!(range, FileIOReadRange::Lines { .. });
        let task = tokio::spawn(async move {
            handler
                .request(read(range), FileIOCancellation::default())
                .await
        });
        let frame = line(&mut lines).await;
        assert_eq!(frame["method"], "fs/read_text_file");
        let mut expected = json!({"sessionId":"acp-session","path":"/editor/unsaved.txt"});
        if ranged {
            expected["line"] = json!(3);
            expected["limit"] = json!(7);
        }
        assert_eq!(frame["params"], expected);
        writer.resolve_checked(frame["id"].as_u64().unwrap(), Ok(json!({"content":""})));
        assert!(
            matches!(task.await.unwrap().unwrap(),FileIOResponse::Text { content } if content.is_empty())
        );
        assert_eq!(writer.pending_count(), 0);
    }
}

#[tokio::test]
async fn write_object_and_null_success_are_distinct_from_rpc_error() {
    for result in [
        Ok(json!({})),
        Ok(Value::Null),
        Err(ClientRequestError::Rpc {
            code: -32002,
            message: "".into(),
        }),
    ] {
        let (writer, handler, mut lines) = handler();
        let error = result.is_err();
        let task = tokio::spawn(async move {
            handler
                .request(write(), FileIOCancellation::default())
                .await
        });
        let frame = line(&mut lines).await;
        assert_eq!(frame["method"], "fs/write_text_file");
        assert_eq!(
            frame["params"],
            json!({"sessionId":"acp-session","path":"/editor/unsaved.txt","content":""})
        );
        writer.resolve_checked(frame["id"].as_u64().unwrap(), result);
        if error {
            assert!(matches!(
                task.await.unwrap(),
                Err(FileIOError {
                    code: FileIOErrorCode::NotFound,
                    ..
                })
            ));
        } else {
            assert!(matches!(task.await.unwrap(), Ok(FileIOResponse::Written)));
        }
    }
}

#[tokio::test]
async fn file_cancel_drop_late_reply_and_disconnect_retire_pending() {
    let (writer, handler, mut lines) = handler();
    let cancel = FileIOCancellation::default();
    let task = tokio::spawn({
        let handler = handler.clone();
        let cancel = cancel.clone();
        async move {
            handler
                .request(read(FileIOReadRange::Complete), cancel)
                .await
        }
    });
    let frame = line(&mut lines).await;
    cancel.cancel();
    assert!(matches!(
        task.await.unwrap(),
        Err(FileIOError {
            code: FileIOErrorCode::Cancelled,
            ..
        })
    ));
    assert_eq!(writer.pending_count(), 0);
    writer.resolve_checked(frame["id"].as_u64().unwrap(), Ok(json!({"content":"late"})));
    let task = tokio::spawn(async move {
        handler
            .request(write(), FileIOCancellation::default())
            .await
    });
    line(&mut lines).await;
    task.abort();
    let _ = task.await;
    assert_eq!(writer.pending_count(), 0);
    writer.close();
    assert_eq!(
        writer.request_checked("x", json!({}), None).await,
        Err(ClientRequestError::Disconnected)
    );
}

#[tokio::test]
async fn pending_cap_is_shared_by_checked_and_legacy_requests() {
    let (writer, _handler, mut lines) = handler();
    let mut tasks = Vec::new();
    for _ in 0..64 {
        tasks.push(tokio::spawn({
            let writer = writer.clone();
            async move { writer.request("dialog", json!({})).await }
        }));
        line(&mut lines).await;
    }
    assert_eq!(writer.pending_count(), 64);
    assert_eq!(
        writer
            .request_checked("fs/read_text_file", json!({}), Some(10))
            .await,
        Err(ClientRequestError::Capacity)
    );
    writer.close();
    for task in tasks {
        assert_eq!(task.await.unwrap(), Value::Null);
    }
    assert_eq!(writer.pending_count(), 0);
}

#[tokio::test]
async fn oversized_client_result_and_text_are_explicit_errors() {
    for content in [
        "x".repeat(MAX_TEXT_BYTES + 1),
        "x".repeat(MAX_PAYLOAD_BYTES + 1),
    ] {
        let (writer, handler, mut lines) = handler();
        let task = tokio::spawn(async move {
            handler
                .request(
                    read(FileIOReadRange::Complete),
                    FileIOCancellation::default(),
                )
                .await
        });
        let frame = line(&mut lines).await;
        writer.resolve_checked(
            frame["id"].as_u64().unwrap(),
            Ok(json!({"content":content})),
        );
        assert!(matches!(
            task.await.unwrap(),
            Err(FileIOError {
                code: FileIOErrorCode::TooLarge,
                ..
            })
        ));
        assert_eq!(writer.pending_count(), 0);
    }
}

#[tokio::test]
async fn cancellation_during_partial_file_frame_fails_transport_closed() {
    let (output, _peer) = tokio::io::duplex(1);
    let writer = Arc::new(Writer::with_output(output));
    let task = tokio::spawn({
        let writer = writer.clone();
        async move {
            writer
                .request_checked("fs/write_text_file", json!({"content":"long"}), None)
                .await
        }
    });
    while writer.pending_count() == 0 {
        tokio::task::yield_now().await;
    }
    // Let the frame start and fill the one-byte output pipe.
    tokio::time::sleep(Duration::from_millis(10)).await;
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(1), writer.terminal())
        .await
        .unwrap();
    assert_eq!(writer.pending_count(), 0);
}

#[test]
fn error_codes_and_serialized_budget_are_bounded() {
    for (code, expected) in [
        (-32000, FileIOErrorCode::Denied),
        (-32002, FileIOErrorCode::NotFound),
        (-32601, FileIOErrorCode::Unsupported),
        (-32800, FileIOErrorCode::Cancelled),
        (-12345, FileIOErrorCode::InvalidResponse),
    ] {
        let error = map_error(ClientRequestError::Rpc {
            code,
            message: "é".repeat(2048),
        });
        assert_eq!(error.code, expected);
        assert!(error.message.len() <= 4096);
    }
    assert!(within_json_budget(&json!(""), 2));
    assert!(!within_json_budget(&json!("\n"), 3));
}
