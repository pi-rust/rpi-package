mod file_io_runtime_tests {
    use super::*;
    use crate::file_io::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::DuplexStream;

    struct Handler {
        calls: mpsc::UnboundedSender<(FileIORequest, FileIOCancellation)>,
        result: StdMutex<Option<Result<FileIOResponse, FileIOError>>>,
        alive: Arc<AtomicUsize>,
    }
    struct Alive(Arc<AtomicUsize>);
    impl Drop for Alive {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl FileIOHandler for Handler {
        fn request(
            &self,
            request: FileIORequest,
            cancel: FileIOCancellation,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<FileIOResponse, FileIOError>> + Send + '_>,
        > {
            Box::pin(async move {
                self.alive.fetch_add(1, Ordering::SeqCst);
                let _alive = Alive(self.alive.clone());
                let _ = self.calls.send((request, cancel));
                let result = self.result.lock().unwrap().clone();
                match result {
                    Some(result) => result,
                    None => std::future::pending().await,
                }
            })
        }
    }
    fn ready() -> Value {
        json!({"type":"ready","protocol":"rpi", "supportedCommands":["configure_file_io","file_io_response"],
            "supportedEvents":["file_io_request","file_io_request_closed"],
            "fileIO":{"version":1,"maxPending":64,"maxTextBytes":262144,"maxPayloadBytes":786432,"timeoutMs":60000}})
    }
    fn request() -> Value {
        json!({"type":"file_io_request","requestId":format!("native:{}",uuid::Uuid::new_v4()),"operation":"read_text_file","path":"/client/a","maxBytes":262144})
    }
    fn closed(value: &Value) -> Value {
        json!({"type":"file_io_request_closed","requestId":value["requestId"]})
    }
    async fn line(reader: &mut BufReader<DuplexStream>) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn ack(events: &TestEvents, command: &Value) {
        let _ = events.send(json!({"type":"response","command":command["type"],"id":command["id"],"success":true,"data":{"accepted":true}}));
    }
    async fn fixture(
        result: Option<Result<FileIOResponse, FileIOError>>,
    ) -> (
        Arc<HostSession>,
        TestEvents,
        BufReader<DuplexStream>,
        mpsc::UnboundedReceiver<(FileIORequest, FileIOCancellation)>,
        Arc<Handler>,
    ) {
        let (writer, reader) = tokio::io::duplex(2 * 1024 * 1024);
        let (host, events) = HostSession::test_connection_with_ready(writer, 1, ready());
        let (calls, receive) = mpsc::unbounded_channel();
        let handler = Arc::new(Handler {
            calls,
            result: StdMutex::new(result),
            alive: Arc::new(AtomicUsize::new(0)),
        });
        let configure = {
            let host = host.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                host.configure_file_io(
                    FileIOCapabilities {
                        read_text: true,
                        write_text: true,
                    },
                    handler,
                )
                .await
            })
        };
        let mut reader = BufReader::new(reader);
        let config = line(&mut reader).await;
        assert_eq!(config["type"], "configure_file_io");
        assert_eq!(config["readText"], true);
        assert_eq!(config["writeText"], true);
        ack(&events, &config);
        configure.await.unwrap().unwrap();
        (host, events, reader, receive, handler)
    }
    async fn drained(handler: &Handler) {
        for _ in 0..100 {
            if handler.alive.load(Ordering::SeqCst) == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("handler future not dropped");
    }

    #[tokio::test]
    async fn old_peer_gating_idle_lease_and_no_handler_fail_closed() {
        let (writer, _) = tokio::io::duplex(4096);
        let (host, events) = HostSession::test_connection_with_ready(writer, 8, json!({}));
        let (calls, _) = mpsc::unbounded_channel();
        let handler = Arc::new(Handler {
            calls,
            result: StdMutex::new(None),
            alive: Arc::new(AtomicUsize::new(0)),
        });
        assert!(host
            .configure_file_io(FileIOCapabilities::default(), handler.clone())
            .await
            .is_err());
        assert!(!*host.runtime.closed_signal.borrow());
        let lease = host.try_run_lease().unwrap();
        assert!(host
            .configure_file_io(FileIOCapabilities::default(), handler)
            .await
            .is_err());
        drop(lease);
        assert!(events.send(request()).is_err());
        assert!(*host.runtime.closed_signal.borrow());
        host.terminate().await;
    }

    #[tokio::test]
    async fn reliable_routing_precedes_laggy_broadcast_and_ack_is_correlated() {
        let (host, events, mut reader, mut calls, _) = fixture(Some(Ok(FileIOResponse::Text {
            content: "client data".into(),
        })))
        .await;
        let mut raw = host.subscribe_raw();
        for _ in 0..5 {
            let _ = events.send(json!({"type":"inert"}));
        }
        let req = request();
        let _ = events.send(req.clone());
        let (typed, _) = calls.recv().await.unwrap();
        assert_eq!(typed.request_id, req["requestId"]);
        assert!(matches!(
            typed.operation,
            FileIOOperation::ReadText {
                range: FileIOReadRange::Complete,
                ..
            }
        ));
        assert!(matches!(
            raw.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        let response = line(&mut reader).await;
        assert_eq!(response["result"], json!({"content":"client data"}));
        assert_eq!(response["requestId"], req["requestId"]);
        assert!(response["id"].is_string());
        ack(&events, &response);
        let _ = events.send(closed(&req));
        host.terminate().await;
    }

    #[tokio::test]
    async fn closure_cancels_handler_late_closure_does_not_cancel_next_request() {
        let (host, events, _, mut calls, handler) = fixture(None).await;
        let first = request();
        let _ = events.send(first.clone());
        let (_, cancel) = calls.recv().await.unwrap();
        let _ = events.send(closed(&first));
        cancel.cancelled().await;
        drained(&handler).await;
        let second = request();
        let _ = events.send(second.clone());
        let (_, next) = calls.recv().await.unwrap();
        let _ = events.send(closed(&first));
        assert!(!next.is_cancelled());
        let _ = events.send(closed(&second));
        host.terminate().await;
        drained(&handler).await;
    }

    #[tokio::test]
    async fn duplicate_and_overflow_are_authoritative_failures() {
        for duplicate in [true, false] {
            let (host, events, _, mut calls, handler) = fixture(None).await;
            let req = request();
            let _ = events.send(req.clone());
            calls.recv().await.unwrap();
            if duplicate {
                assert!(events.send(req).is_err());
            } else {
                for _ in 1..MAX_PENDING {
                    let _ = events.send(request());
                }
                assert!(events.send(request()).is_err());
            }
            assert!(*host.runtime.closed_signal.borrow());
            host.terminate().await;
            drained(&handler).await;
        }
    }

    #[tokio::test]
    async fn resolved_but_native_unconsumed_replies_still_hold_all_64_slots() {
        let (host, events, mut reader, _, _) =
            fixture(Some(Ok(FileIOResponse::Text { content: "".into() }))).await;
        for _ in 0..MAX_PENDING {
            let _ = events.send(request());
            let response = line(&mut reader).await;
            ack(&events, &response);
        }
        assert!(events.send(request()).is_err());
        assert!(*host.runtime.closed_signal.borrow());
        host.terminate().await;
    }

    #[tokio::test]
    async fn abort_cancels_calls_but_keeps_registration() {
        let (host, events, mut reader, mut calls, handler) = fixture(None).await;
        let req = request();
        let _ = events.send(req.clone());
        let (_, cancel) = calls.recv().await.unwrap();
        host.abort_run().await.unwrap();
        assert_eq!(line(&mut reader).await["type"], "abort");
        cancel.cancelled().await;
        let _ = events.send(closed(&req));
        drained(&handler).await;
        let req = request();
        let _ = events.send(req.clone());
        calls.recv().await.unwrap();
        assert!(!*host.runtime.closed_signal.borrow());
        let _ = events.send(closed(&req));
        host.terminate().await;
    }

    #[tokio::test]
    async fn owner_drop_is_weak_and_disconnect_joins_handler_futures() {
        for owner_drop in [true, false] {
            let (host, events, _, mut calls, handler) = fixture(None).await;
            let _ = events.send(request());
            let (_, cancel) = calls.recv().await.unwrap();
            let weak = Arc::downgrade(&host);
            drop(events);
            if owner_drop {
                drop(host);
                assert!(weak.upgrade().is_none());
            } else {
                host.runtime.finish().await;
                host.terminate().await;
            }
            cancel.cancelled().await;
            drained(&handler).await;
        }
    }

    #[tokio::test]
    async fn configuration_future_drop_fails_closed_and_releases_lease() {
        let (writer, reader) = tokio::io::duplex(4096);
        let (host, _events) = HostSession::test_connection_with_ready(writer, 8, ready());
        let (calls, _) = mpsc::unbounded_channel();
        let handler = Arc::new(Handler {
            calls,
            result: StdMutex::new(None),
            alive: Arc::new(AtomicUsize::new(0)),
        });
        let task = {
            let host = host.clone();
            tokio::spawn(async move {
                host.configure_file_io(
                    FileIOCapabilities {
                        read_text: true,
                        write_text: false,
                    },
                    handler,
                )
                .await
            })
        };
        line(&mut BufReader::new(reader)).await;
        task.abort();
        let _ = task.await;
        assert!(*host.runtime.closed_signal.borrow());
        assert!(host.try_run_lease().is_ok());
        host.terminate().await;
    }

    #[tokio::test(start_paused = true)]
    async fn core_deadline_returns_timeout_before_native_60_second_deadline() {
        let (host, events, mut reader, mut calls, handler) = fixture(None).await;
        let req = request();
        let _ = events.send(req.clone());
        calls.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(50)).await;
        let response = line(&mut reader).await;
        assert_eq!(response["error"]["code"], "timeout");
        ack(&events, &response);
        let _ = events.send(closed(&req));
        host.terminate().await;
        drained(&handler).await;
    }

    #[tokio::test]
    async fn prompt_profile_gate_order_and_legacy_limits_are_independent() {
        let (writer, reader) = tokio::io::duplex(2 * 1024 * 1024);
        let (host, _) = HostSession::test_connection_with_ready(
            writer,
            8,
            json!({"semantics":{"messageIds":"sessionEntryId-v1"}}),
        );
        assert!(host.supports_message_ids());
        let content = crate::prompt::PromptContent::new(vec![crate::prompt::PromptBlock::Text {
            text: "ordered".into(),
        }])
        .unwrap();
        assert!(host.begin_prompt_content(content).await.is_err());
        let run = host
            .begin_prompt("x".repeat(crate::prompt::MAX_TEXT_BYTES + 1))
            .await
            .unwrap();
        let frame = line(&mut BufReader::new(reader)).await;
        assert_eq!(frame["type"], "prompt");
        assert!(frame.get("contentBlocks").is_none());
        assert_eq!(
            frame["message"].as_str().unwrap().len(),
            crate::prompt::MAX_TEXT_BYTES + 1
        );
        drop(run);
        host.terminate().await;
    }

    fn prompt_profile() -> Value {
        json!({"contentBlocksVersion":1,"image":true,"imageMimeTypes":["image/png","image/jpeg"],
            "maxImages":4,"maxImageBytes":262144,"maxTotalImageBytes":524288,"maxImageBase64Bytes":349528,
            "maxPromptBytes":786432,"maxTextBytes":32768,"maxContentBlocks":64,"maxImageSide":2000,
            "maxImagePixels":4000000,"imageDecodeAllocBudgetBytes":33554432,"frameBytes":1048576})
    }

    #[tokio::test]
    async fn ordered_prompt_keeps_image_order_and_checks_final_uuid_type_and_lf() {
        use crate::prompt::{PromptBlock, PromptContent, MAX_PROMPT_BYTES};
        let (writer, reader) = tokio::io::duplex(2 * 1024 * 1024);
        let (host, _) = HostSession::test_connection_with_ready(
            writer,
            8,
            json!({"promptContent":prompt_profile()}),
        );
        let content = PromptContent::new(vec![
            PromptBlock::Text {
                text: "before".into(),
            },
            PromptBlock::Image {
                data: "/9j/4A==".into(),
                mime_type: "image/jpeg".into(),
            },
            PromptBlock::Text {
                text: "after".into(),
            },
        ])
        .unwrap();
        let mut reader = BufReader::new(reader);
        let run = host.begin_prompt_content(content.clone()).await.unwrap();
        let frame = line(&mut reader).await;
        assert_eq!(frame["contentBlocks"], json!(content));
        assert!(frame.get("message").is_none());
        assert!(uuid::Uuid::parse_str(frame["id"].as_str().unwrap()).is_ok());
        drop(run);
        let mut command = json!({"contentBlocks":content,"extensionPadding":""});
        let mut envelope = command.clone();
        envelope["type"] = json!("prompt");
        envelope["id"] = json!("0".repeat(36));
        let base = serde_json::to_vec(&envelope).unwrap().len() + 1;
        command["extensionPadding"] = json!("x".repeat(MAX_PROMPT_BYTES - base + 1));
        assert!(host
            .begin_operation(crate::run::RunKind::Prompt, command.clone())
            .await
            .is_err());
        assert!(host.try_run_lease().is_ok());
        command["extensionPadding"] = json!("x".repeat(MAX_PROMPT_BYTES - base));
        let run = host
            .begin_operation(crate::run::RunKind::Prompt, command)
            .await
            .unwrap();
        let frame = line(&mut reader).await;
        assert_eq!(
            serde_json::to_vec(&frame).unwrap().len() + 1,
            MAX_PROMPT_BYTES
        );
        drop(run);
        host.terminate().await;
    }

    #[tokio::test(start_paused = true)]
    async fn configuration_timeout_rejection_and_null_ack_fail_closed() {
        for outcome in [None, Some(false), Some(true)] {
            let (writer, reader) = tokio::io::duplex(4096);
            let (host, events) = HostSession::test_connection_with_ready(writer, 8, ready());
            let (calls, _) = mpsc::unbounded_channel();
            let handler = Arc::new(Handler {
                calls,
                result: StdMutex::new(None),
                alive: Arc::new(AtomicUsize::new(0)),
            });
            let task = {
                let host = host.clone();
                tokio::spawn(async move {
                    host.configure_file_io(FileIOCapabilities::default(), handler)
                        .await
                })
            };
            let config = line(&mut BufReader::new(reader)).await;
            match outcome {
                None => tokio::time::advance(Duration::from_secs(5)).await,
                Some(false) => {
                    let _ =
                        events.send(json!({"type":"response","id":config["id"],"success":false}));
                }
                Some(true) => {
                    let _ =
                        events.send(json!({"type":"response","id":config["id"],"success":null}));
                }
            }
            assert!(task.await.unwrap().is_err());
            assert!(*host.runtime.closed_signal.borrow());
            host.terminate().await;
        }
    }
}
