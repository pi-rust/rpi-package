use super::*;
#[tokio::test]
async fn validated_boundary_blocked_retry_reuses_pending_result_reservation() {
    let f = Fixture::new(false);
    let mut r = record();
    let fingerprint = "a".repeat(64);
    let superseded_request = ApplyResourcesRequest {
        expected_revision: 1,
        operation_id: "prior-typo".into(),
        target: selection(),
        supersedes_operation_id: None,
    };
    r.operations.push(ResourceOperation {
        transition: ResourceTransition {
            version: 1,
            operation_id: "prior-typo".into(),
            expected: ExpectedBinding {
                revision: 1,
                fingerprint: Some(fingerprint.clone()),
            },
            target: selection(),
        },
        request: superseded_request,
        restore_settings: None,
        status: ResourceOperationStatus::Superseded,
        result_revision: None,
        observation: None,
        error: None,
    });
    for revision in 1..=5 {
        let request = ApplyResourcesRequest {
            expected_revision: revision,
            operation_id: format!("applied-{revision}"),
            target: selection(),
            supersedes_operation_id: (revision == 1).then(|| "prior-typo".into()),
        };
        let observation = ResourceObservation::parse(json!({"version":1,"hostId":"saved-host","sessionId":"source","binding":{"revision":revision+1,"fingerprint":fingerprint,"operationId":request.operation_id},"selection":selection(),"loadedSkills":[],"loadedExtensions":[],"registeredTools":[],"modelEffectiveTools":[],"padding":"x".repeat(240_000)})).unwrap();
        r.operations.push(ResourceOperation {
            transition: ResourceTransition {
                version: 1,
                operation_id: request.operation_id.clone(),
                expected: ExpectedBinding {
                    revision,
                    fingerprint: Some(fingerprint.clone()),
                },
                target: selection(),
            },
            request,
            restore_settings: None,
            status: ResourceOperationStatus::Applied,
            result_revision: Some(revision + 1),
            observation: Some(observation.clone()),
            error: None,
        });
        r.observation = Some(observation);
        r.revision = revision + 1;
    }
    let request = ApplyResourcesRequest {
        expected_revision: 6,
        operation_id: "retry-boundary".into(),
        target: selection(),
        supersedes_operation_id: None,
    };
    r.operations.push(ResourceOperation {
        transition: ResourceTransition {
            version: 1,
            operation_id: request.operation_id.clone(),
            expected: ExpectedBinding {
                revision: 6,
                fingerprint: Some(fingerprint),
            },
            target: selection(),
        },
        request: request.clone(),
        restore_settings: None,
        status: ResourceOperationStatus::Pending,
        result_revision: None,
        observation: None,
        error: None,
    });
    let threshold = 2 * 1024 * 1024 - (2 * MAX_STATE_BYTES + MAX_CONFIG_BYTES + 32 * 1024);
    for i in 1..5 {
        let remaining = threshold - serde_json::to_vec(&r).unwrap().len();
        let observation = r.operations[i].observation.as_mut().unwrap();
        let pad = observation.body["padding"].as_str().unwrap().len();
        observation.body["padding"] = json!("x".repeat(pad + remaining.min(10_000)));
    }
    assert_eq!(serde_json::to_vec(&r).unwrap().len(), threshold);
    let mut registry = f.disk();
    registry
        .set_resource_configuration(entry("source"), r.clone())
        .unwrap();
    // A validated boundary-sized Pending reaches saved-reader discovery,
    // NOT a ledger-sizing rejection.
    let config = f.owner.config.clone();
    let pending_owner = Service::new(config.clone()).unwrap();
    let pending = pending_owner
        .apply_resources("source".into(), request.clone())
        .await
        .unwrap_err();
    pending_owner.shutdown().await;
    r.operations.last_mut().unwrap().status = ResourceOperationStatus::Blocked;
    r.operations.last_mut().unwrap().error = Some("bounded failure diagnostic".into());
    registry
        .set_resource_configuration(entry("source"), r.clone())
        .unwrap();
    let owner = Service::new(config.clone()).unwrap();
    let blocked = owner
        .apply_resources("source".into(), request.clone())
        .await
        .unwrap_err();
    owner.shutdown().await;
    // Pending plus one diagnostic is still within the final 2 MiB cap; only
    // that latest diagnostic is replaceable, NOT earlier charged diagnostics.
    r.operations[0].error = Some("historical diagnostic must remain charged".into());
    registry
        .set_resource_configuration(entry("source"), r.clone())
        .unwrap();
    let historical_owner = Service::new(config.clone()).unwrap();
    let historical = historical_owner
        .apply_resources("source".into(), request.clone())
        .await
        .unwrap_err();
    historical_owner.shutdown().await;
    r.operations[0].error = None;
    // Persisted records with genuinely unreserved payload must still refuse.
    r.operations[1].observation.as_mut().unwrap().body["padding"] = json!("x".repeat(255_000));
    registry
        .set_resource_configuration(entry("source"), r)
        .unwrap();
    let oversized_owner = Service::new(config).unwrap();
    let oversized = oversized_owner
        .apply_resources("source".into(), request)
        .await
        .unwrap_err();
    oversized_owner.shutdown().await;
    f.end().await;
    assert_eq!(pending.kind, ResourceErrorKind::Unavailable, "{pending:?}");
    assert_eq!(
        blocked.kind, pending.kind,
        "failure metadata must not revoke admitted reservation: {blocked:?}"
    );
    assert_eq!(historical.kind, ResourceErrorKind::LimitExceeded);
    assert_eq!(oversized.kind, ResourceErrorKind::LimitExceeded);
}
