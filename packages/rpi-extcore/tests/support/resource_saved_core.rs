use super::*;
fn transcript(config: &BackendConfig, id: &str) -> PathBuf {
    for dir in std::fs::read_dir(config.session_dir.as_ref().unwrap()).unwrap() {
        let dir = dir.unwrap();
        if !dir.file_type().unwrap().is_dir() {
            continue;
        }
        for path in std::fs::read_dir(dir.path()).unwrap() {
            let path = path.unwrap().path();
            if path.to_string_lossy().ends_with(&format!("_{id}.jsonl")) {
                return path;
            }
        }
    }
    panic!("missing native transcript {id}")
}
pub async fn verify(
    service: &Service,
    id: &str,
    project_id: &str,
    config: &BackendConfig,
) -> Arc<Service> {
    let view = service
        .resource_saved_history(
            id.into(),
            ResourceHistoryRequest {
                limit: 2,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(!view.executable);
    assert!(view.filtered);
    assert_eq!(view.messages.len(), 2);
    assert!(view.cursor.is_some());
    let next = service
        .resource_saved_history(
            id.into(),
            ResourceHistoryRequest {
                limit: 2,
                cursor: view.cursor.clone(),
            },
        )
        .await
        .unwrap();
    assert_ne!(view.messages[0]["entryId"], next.messages[0]["entryId"]);
    let path = transcript(config, id);
    let original = std::fs::read(&path).unwrap();
    let seq = String::from_utf8(original.clone())
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).unwrap()["seq"].as_u64())
        .max()
        .unwrap()
        + 1;
    let before_ledger = service.resource_operations(id).await.unwrap().len();
    let revision = service
        .resource_configuration(id)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let none = ResourceSelection {
        version: 1,
        main_package: None,
        skills: vec![],
        extensions: vec![],
        instructions: vec![],
        tools: ResourceTools::None {},
    };
    let request = ApplyResourcesRequest {
        expected_revision: revision,
        operation_id: "must-not-run-native-pending".into(),
        target: none.clone(),
        supersedes_operation_id: None,
    };
    let custom = json!({"kind":"entry","type":"custom","id":"core-opaque-proof","seq":seq,"timestamp":0,"lane":"main","parentId":view.leaf_id,"customType":"opaque-test","data":{"opaque":true}});
    let opaque_bytes = [
        original.clone(),
        format!(
            "{custom}
"
        )
        .into_bytes(),
    ]
    .concat();
    put(&path, &opaque_bytes);
    let opaque_view = service
        .resource_saved_history(id.into(), ResourceHistoryRequest::default())
        .await
        .unwrap();
    // Unknown custom entries are not interpreted or rejected by the package
    // adapter: the native saved-read query does not request any custom type.
    assert!(opaque_view.custom_entries.is_empty());
    assert!(opaque_view.executable);
    assert_eq!(
        service.resource_operations(id).await.unwrap().len(),
        before_ledger
    );
    let queue_view;
    // Test-owned source reset; production has no repair/write API.
    put(&path, &original);
    let queued = json!({"kind":"record","type":"queue_enqueued","id":"native-pending-queue","seq":seq,"timestamp":0,"lane":"main","queue":"nextRun","target":{"type":"message","id":"not-yet-committed","message":{"kind":"user","role":"user","content":"must stay pending","timestamp":0}}});
    let native_pending = [original.clone(), format!("{queued}\n").into_bytes()].concat();
    put(&path, &native_pending);
    let progress_path = PathBuf::from(format!("{}.progress.jsonl", path.display()));
    let prior_progress = std::fs::read(&progress_path).unwrap();
    let progress=json!({"format":"assistant-progress-v1","session_id":id,"state":{"sequence":1,"frames":[{"id":"interrupted-frame","seq":1,"timestamp":0,"lane":"main","runId":"interrupted-native","streamIndex":0,"op":"append","frame":{"type":"text_delta","delta":"unfinished native output"}}]}}).to_string()+"\n";
    put(&progress_path, &progress);
    let pending_view = service
        .resource_saved_history(id.into(), ResourceHistoryRequest::default())
        .await
        .unwrap();
    assert!(pending_view.pending.execution_queue_or_recovery);
    assert!(pending_view.pending.progress);
    assert!(!pending_view.executable);
    let mut pending_request = request.clone();
    pending_request.operation_id = "must-not-run-native-pending".into();
    assert_eq!(
        service
            .apply_resources(id.into(), pending_request)
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Conflict
    );
    assert_eq!(std::fs::read(&path).unwrap(), native_pending);
    assert_eq!(std::fs::read_to_string(&progress_path).unwrap(), progress);
    assert_eq!(
        service.resource_operations(id).await.unwrap().len(),
        before_ledger
    );
    put(&path, &original);
    put(&progress_path, &prior_progress);
    let mut typo = request;
    typo.operation_id = "tool-typo".into();
    typo.target.tools = ResourceTools::Only {
        names: vec!["definitely-not-a-native-tool".into()],
    };
    let failed = service
        .apply_resources(id.into(), typo.clone())
        .await
        .unwrap();
    assert_eq!(
        failed.status,
        ResourceOperationStatus::Blocked,
        "{failed:?}"
    );
    let current = service.resource_configuration(id).await.unwrap().unwrap();
    assert_eq!(current.revision, revision);
    assert_eq!(current.selection, none);
    queue_view = service
        .resource_saved_history(id.into(), ResourceHistoryRequest::default())
        .await
        .unwrap();
    assert_eq!(
        queue_view.blocked_operation_id.as_deref(),
        Some("tool-typo")
    );
    assert!(serde_json::to_string(&queue_view.messages)
        .unwrap()
        .contains("prior real A message"));
    let repair = ApplyResourcesRequest {
        expected_revision: revision,
        operation_id: "explicit-correction".into(),
        target: none.clone(),
        supersedes_operation_id: Some("tool-typo".into()),
    };
    for mut wrong in [
        ApplyResourcesRequest {
            supersedes_operation_id: None,
            ..repair.clone()
        },
        ApplyResourcesRequest {
            supersedes_operation_id: Some("wrong-id".into()),
            ..repair.clone()
        },
        ApplyResourcesRequest {
            expected_revision: revision - 1,
            ..repair.clone()
        },
    ] {
        wrong.operation_id = format!("negative-{}", uuid::Uuid::new_v4());
        assert_eq!(
            service
                .apply_resources(id.into(), wrong)
                .await
                .unwrap_err()
                .kind,
            ResourceErrorKind::Conflict
        );
    }
    let applied = service
        .apply_resources(id.into(), repair.clone())
        .await
        .unwrap();
    assert_eq!(
        applied.status,
        ResourceOperationStatus::Applied,
        "{applied:?}"
    );
    assert_eq!(applied.result_revision, Some(revision + 1));
    let terminal = service
        .apply_resources(id.into(), typo.clone())
        .await
        .unwrap();
    assert_eq!(terminal.status, ResourceOperationStatus::Superseded);
    assert_eq!(terminal.request, typo);
    assert_eq!(terminal.transition, failed.transition);
    let mut changed = typo;
    changed.target = none.clone();
    assert_eq!(
        service
            .apply_resources(id.into(), changed)
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Conflict
    );
    assert_eq!(
        service
            .apply_resources(id.into(), repair)
            .await
            .unwrap()
            .result_revision,
        Some(revision + 1)
    );
    let captured = service.live_session(id).await.unwrap();
    let cancel_request = ApplyResourcesRequest {
        expected_revision: revision + 1,
        operation_id: "cancelled-caller-retained-owner".into(),
        target: none.clone(),
        supersedes_operation_id: None,
    };
    let caller_owner = service.clone();
    let caller_id = id.to_owned();
    let caller_request = cancel_request.clone();
    let caller = tokio::spawn(async move {
        caller_owner
            .apply_resources(caller_id, caller_request)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(operation) = service
                .resource_operation(id, &cancel_request.operation_id)
                .await
                .unwrap()
            {
                assert_eq!(
                    operation.status,
                    ResourceOperationStatus::Pending,
                    "test must observe retained owner mid-apply"
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    let _ = caller.await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let operation = service
                .resource_operation(id, &cancel_request.operation_id)
                .await
                .unwrap()
                .unwrap();
            if operation.status != ResourceOperationStatus::Pending {
                assert_eq!(
                    operation.status,
                    ResourceOperationStatus::Applied,
                    "{operation:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(captured
        .begin_prompt("cancelled caller cannot revive old Arc".into())
        .await
        .is_err());
    assert_eq!(
        service
            .apply_resources(id.into(), cancel_request)
            .await
            .unwrap()
            .result_revision,
        Some(revision + 2)
    );
    service
        .release_idle_resources(project_id.into(), ResourceScope::Project)
        .await
        .unwrap();
    // Legacy failed opt-in remains only an operation journal, never a fake
    // revision-zero active configuration. Correction stays explicit.
    let (kind, legacy) = service::create_session(
        service,
        CreateSessionRequest {
            project_id: None,
            resources: None,
        },
    )
    .await;
    assert_eq!(kind, ReplyKind::Created, "{legacy}");
    let legacy_id = legacy["sessionId"].as_str().unwrap();
    let bad = ApplyResourcesRequest {
        expected_revision: 0,
        operation_id: "legacy-typo".into(),
        target: ResourceSelection {
            tools: ResourceTools::Only {
                names: vec!["definitely-not-native".into()],
            },
            ..none.clone()
        },
        supersedes_operation_id: None,
    };
    assert_eq!(
        service
            .apply_resources(legacy_id.into(), bad.clone())
            .await
            .unwrap()
            .status,
        ResourceOperationStatus::Blocked
    );
    assert!(service
        .resource_configuration(legacy_id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        service
            .resource_operation(legacy_id, "legacy-typo")
            .await
            .unwrap()
            .unwrap()
            .request,
        bad
    );
    assert!(
        !service
            .resource_saved_history(legacy_id.into(), ResourceHistoryRequest::default())
            .await
            .unwrap()
            .executable
    );
    let fixed = ApplyResourcesRequest {
        expected_revision: 0,
        operation_id: "legacy-corrected".into(),
        target: none.clone(),
        supersedes_operation_id: Some("legacy-typo".into()),
    };
    let applied = service
        .apply_resources(legacy_id.into(), fixed)
        .await
        .unwrap();
    assert_eq!(
        applied.status,
        ResourceOperationStatus::Applied,
        "{applied:?}"
    );
    assert_eq!(applied.result_revision, Some(1));
    service
        .release_idle_resources(project_id.into(), ResourceScope::Project)
        .await
        .unwrap();
    service.shutdown().await;
    // Fault fixture: independently preserve the real native commit, but lose
    // only application completion publication. Production never rewrites native
    // history to manufacture recovery. Supersede MUST refuse this state.
    let mut journal: Value =
        serde_json::from_slice(&std::fs::read(&config.registry).unwrap()).unwrap();
    let record = &mut journal["resources"][id];
    let old_observation = record["operations"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .skip(1)
        .find(|op| op["status"] == "applied")
        .unwrap()["observation"]
        .clone();
    let last = record["operations"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    let original_request: ApplyResourcesRequest =
        serde_json::from_value(last["request"].clone()).unwrap();
    last["status"] = json!("pending");
    last["result_revision"] = Value::Null;
    last["observation"] = Value::Null;
    last["error"] = Value::Null;
    record["revision"] = json!(original_request.expected_revision);
    record["observation"] = old_observation;
    put(&config.registry, serde_json::to_vec(&journal).unwrap());
    let recovered = Service::new(config.clone()).unwrap();
    let view = recovered
        .resource_saved_history(id.into(), ResourceHistoryRequest::default())
        .await
        .unwrap();
    assert_eq!(
        view.binding.operation_id.as_deref(),
        Some(original_request.operation_id.as_str())
    );
    let attempt = ApplyResourcesRequest {
        expected_revision: original_request.expected_revision,
        operation_id: "unsafe-supersede-native-commit".into(),
        target: none,
        supersedes_operation_id: Some(original_request.operation_id.clone()),
    };
    let journal_before = std::fs::read(&config.registry).unwrap();
    assert_eq!(
        recovered
            .apply_resources(id.into(), attempt)
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Conflict
    );
    assert_eq!(std::fs::read(&config.registry).unwrap(), journal_before);
    let result = recovered
        .apply_resources(id.into(), original_request)
        .await
        .unwrap();
    assert_eq!(
        result.status,
        ResourceOperationStatus::Applied,
        "{result:?}"
    );
    assert_eq!(
        result.observation.unwrap().binding.revision,
        view.binding.revision,
        "original native commit is replayed, never applied twice"
    );
    assert_eq!(
        recovered
            .resource_operation(id, "tool-typo")
            .await
            .unwrap()
            .unwrap()
            .status,
        ResourceOperationStatus::Superseded
    );
    recovered
        .release_idle_resources(project_id.into(), ResourceScope::Project)
        .await
        .unwrap();
    println!("canonical saved/blocked history, native idle refusal, explicit terminal supersede, opaque custom-entry preservation and committed-native/original-ID recovery acceptance passed");
    recovered
}
