use super::*;
use serde_json::json;
use std::sync::{
    Barrier,
    atomic::{AtomicUsize, Ordering},
};

fn registry(entries: usize, bytes: usize) -> RemoteOperations {
    RemoteOperations::with_limits(Limits {
        entries,
        result_bytes: bytes,
    })
    .unwrap()
}
fn reserve(registry: &RemoteOperations) -> OperationRef {
    registry
        .reserve(
            "principal",
            "client-1",
            "session",
            RealtimeOperationKind::SessionCommand,
        )
        .unwrap()
}
fn execute(
    registry: &RemoteOperations,
    reference: &OperationRef,
    payload: Value,
) -> OperationPermit {
    match registry.claim("principal", reference, payload).unwrap() {
        OperationClaim::Execute(permit) => permit,
        OperationClaim::Existing(_) => panic!("unexpected duplicate"),
    }
}
fn success(value: Value) -> OperationOutcome {
    OperationOutcome::Succeeded { value }
}

#[test]
fn reserve_is_not_acceptance_and_first_payload_is_owned_not_rewritten() {
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    assert_eq!(
        registry.status("principal", &reference).unwrap(),
        OperationStatus::Reserved
    );
    let payload = json!({"type":"prompt","contentBlocks":[{"type":"image","mimeType":"image/png","data":"opaque-base64"}]});
    let permit = execute(&registry, &reference, payload.clone());
    assert_eq!(permit.reference(), &reference);
    assert_eq!(permit.session_id(), "session");
    assert_eq!(permit.client_instance_id(), "client-1");
    assert_eq!(permit.kind(), RealtimeOperationKind::SessionCommand);
    assert_eq!(permit.payload(), &payload);
    assert_eq!(
        registry.status("principal", &reference).unwrap(),
        OperationStatus::Dispatching
    );
    let value = json!({"success":true,"data":{"accepted":true}});
    let expected = OperationStatus::KnownResult {
        outcome: success(value.clone()),
    };
    assert_eq!(permit.finish(success(value)).unwrap(), expected);
    assert_eq!(registry.status("principal", &reference).unwrap(), expected);
    assert!(
        matches!(registry.claim("principal", &reference, payload).unwrap(), OperationClaim::Existing(status) if status == expected)
    );
}

#[test]
fn simultaneous_duplicate_claims_have_exactly_one_permit() {
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    let barrier = Arc::new(Barrier::new(16));
    let executions = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let registry = registry.clone();
            let reference = reference.clone();
            let barrier = barrier.clone();
            let executions = executions.clone();
            std::thread::spawn(move || {
                barrier.wait();
                if let OperationClaim::Execute(permit) = registry
                    .claim("principal", &reference, json!({"type":"prompt"}))
                    .unwrap()
                {
                    executions.fetch_add(1, Ordering::SeqCst);
                    permit.finish(success(json!({"accepted":true}))).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(matches!(
        registry.status("principal", &reference).unwrap(),
        OperationStatus::KnownResult { .. }
    ));
}

#[test]
fn reordered_object_keys_are_duplicates_but_changed_arrays_conflict() {
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    let first = serde_json::from_str(r#"{"type":"prompt","nested":{"z":[1,2],"a":"x"}}"#).unwrap();
    let reordered =
        serde_json::from_str(r#"{"nested":{"a":"x","z":[1,2]},"type":"prompt"}"#).unwrap();
    let permit = execute(&registry, &reference, first);
    assert!(matches!(
        registry.claim("principal", &reference, reordered).unwrap(),
        OperationClaim::Existing(OperationStatus::Dispatching)
    ));
    assert!(matches!(
        registry.claim(
            "principal",
            &reference,
            json!({"type":"prompt","nested":{"a":"x","z":[2,1]}})
        ),
        Err(OperationError::Conflict)
    ));
    drop(permit);
}

#[test]
fn unknown_and_cross_owner_references_never_create_execution() {
    let first = registry(8, 4096);
    let other = registry(8, 4096);
    let reference = reserve(&first);
    assert_ne!(first.owner_epoch(), other.owner_epoch());
    assert!(matches!(
        other.claim("principal", &reference, Value::Null),
        Err(OperationError::UnknownOperation)
    ));
    assert_eq!(
        other.status("principal", &reference).unwrap(),
        OperationStatus::OutcomeUnknown
    );
    let unknown = OperationRef {
        owner_epoch: first.owner_epoch().into(),
        operation_id: "never-issued".into(),
    };
    assert!(matches!(
        first.claim("principal", &unknown, Value::Null),
        Err(OperationError::UnknownOperation)
    ));
    assert_eq!(first.lock().unwrap().entries.len(), 1);
}

#[test]
fn receipts_and_execution_are_bound_to_authenticated_principal() {
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    assert!(matches!(
        registry.claim("other", &reference, Value::Null),
        Err(OperationError::AccessDenied)
    ));
    assert_eq!(
        registry.status("other", &reference),
        Err(OperationError::AccessDenied)
    );
    assert_eq!(
        registry.status("principal", &reference).unwrap(),
        OperationStatus::Reserved
    );
}

#[test]
fn reservation_and_result_expiry_cannot_reauthorize_an_old_id() {
    let registry = registry(8, 4096);
    for completed in [false, true] {
        let reference = reserve(&registry);
        if completed {
            execute(&registry, &reference, Value::Null)
                .finish(success(Value::Null))
                .unwrap();
        }
        registry
            .lock()
            .unwrap()
            .entries
            .get_mut(&reference.operation_id)
            .unwrap()
            .expires_at = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(
            registry.status("principal", &reference).unwrap(),
            OperationStatus::OutcomeUnknown
        );
        assert!(matches!(
            registry.claim("principal", &reference, Value::Null),
            Err(OperationError::UnknownOperation)
        ));
    }
    assert_eq!(registry.lock().unwrap().result_bytes, 0);
}

#[test]
fn dropped_or_panicking_execution_is_uncertain_not_reserved() {
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    let permit = execute(&registry, &reference, Value::Null);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _permit = permit;
        panic!("isolated simulated executor failure");
    }));
    assert_eq!(
        registry.status("principal", &reference).unwrap(),
        OperationStatus::OutcomeUnknown
    );
    assert!(matches!(
        registry
            .claim("principal", &reference, Value::Null)
            .unwrap(),
        OperationClaim::Existing(OperationStatus::OutcomeUnknown)
    ));
}

#[tokio::test]
async fn cancelling_executor_future_does_not_allow_a_second_execution() {
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    let permit = execute(&registry, &reference, Value::Null);
    let task = tokio::spawn(async move {
        let _permit = permit;
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(matches!(
        registry
            .claim("principal", &reference, Value::Null)
            .unwrap(),
        OperationClaim::Existing(OperationStatus::OutcomeUnknown)
    ));
}

#[test]
fn count_budget_rejects_new_work_without_evicting_inflight_or_reserved() {
    let registry = registry(2, 4096);
    let first = reserve(&registry);
    let _permit = execute(&registry, &first, Value::Null);
    let second = reserve(&registry);
    registry
        .lock()
        .unwrap()
        .purge(Instant::now() + Duration::from_secs(3600));
    assert_eq!(
        registry.status("principal", &first).unwrap(),
        OperationStatus::Dispatching
    );
    // The unexecuted reservation expires; executing work never does.
    assert_eq!(
        registry.status("principal", &second).unwrap(),
        OperationStatus::OutcomeUnknown
    );
    let _second = reserve(&registry);
    assert_eq!(
        registry.reserve(
            "principal",
            "client-1",
            "s",
            RealtimeOperationKind::SessionCommand
        ),
        Err(OperationError::LimitExceeded)
    );
}

#[test]
fn completed_entry_eviction_cannot_execute_its_late_retry() {
    let registry = registry(1, 4096);
    let old = reserve(&registry);
    execute(&registry, &old, Value::Null)
        .finish(success(json!("known")))
        .unwrap();
    let new = reserve(&registry);
    assert_ne!(old.operation_id, new.operation_id);
    assert_eq!(
        registry.status("principal", &old).unwrap(),
        OperationStatus::OutcomeUnknown
    );
    assert!(matches!(
        registry.claim("principal", &old, Value::Null),
        Err(OperationError::UnknownOperation)
    ));
    assert_eq!(registry.lock().unwrap().result_bytes, 0);
}

#[test]
fn cached_bytes_are_bounded_and_oversized_results_become_unknown() {
    let registry = registry(8, 128);
    let first = reserve(&registry);
    execute(&registry, &first, Value::Null)
        .finish(success(json!("x".repeat(60))))
        .unwrap();
    let second = reserve(&registry);
    execute(&registry, &second, Value::Null)
        .finish(success(json!("y".repeat(60))))
        .unwrap();
    assert_eq!(
        registry.status("principal", &first).unwrap(),
        OperationStatus::OutcomeUnknown
    );
    let huge = reserve(&registry);
    assert_eq!(
        execute(&registry, &huge, Value::Null)
            .finish(success(json!("z".repeat(1024))))
            .unwrap(),
        OperationStatus::OutcomeUnknown
    );
    assert!(registry.lock().unwrap().result_bytes <= 128);
    assert!(matches!(
        registry.claim("principal", &huge, Value::Null).unwrap(),
        OperationClaim::Existing(OperationStatus::OutcomeUnknown)
    ));
}

#[test]
fn principal_reservation_and_payload_budgets_do_not_allocate_execution() {
    let registry = registry(128, 4096);
    for _ in 0..MAX_RESERVED_PER_PRINCIPAL {
        reserve(&registry);
    }
    assert_eq!(
        registry.reserve(
            "principal",
            "client-1",
            "session",
            RealtimeOperationKind::SessionCommand
        ),
        Err(OperationError::LimitExceeded)
    );
    let other = registry
        .reserve(
            "other",
            "client-2",
            "session",
            RealtimeOperationKind::SessionCommand,
        )
        .unwrap();
    assert!(matches!(
        registry.claim("other", &other, json!("x".repeat(MAX_MESSAGE_BYTES))),
        Err(OperationError::LimitExceeded)
    ));
    assert_eq!(
        registry.status("other", &other).unwrap(),
        OperationStatus::Reserved
    );
    for principal in [
        "".to_owned(),
        "bad\nprincipal".into(),
        "x".repeat(MAX_ID_BYTES + 1),
    ] {
        assert_eq!(
            registry.reserve(
                &principal,
                "client-1",
                "session",
                RealtimeOperationKind::SessionCommand
            ),
            Err(OperationError::InvalidInput)
        );
    }
}

#[test]
fn closing_owner_disables_reservations_but_keeps_dispatched_receipts() {
    let registry = registry(8, 4096);
    let pending = reserve(&registry);
    let active = reserve(&registry);
    let permit = execute(&registry, &active, Value::Null);
    registry.close();
    assert_eq!(
        registry.reserve(
            "principal",
            "client-1",
            "session",
            RealtimeOperationKind::SessionCommand
        ),
        Err(OperationError::Unavailable)
    );
    assert!(matches!(
        registry.claim("principal", &pending, Value::Null),
        Err(OperationError::UnknownOperation)
    ));
    let result = permit.finish(success(json!({"accepted":true}))).unwrap();
    assert_eq!(registry.status("principal", &active).unwrap(), result);
}

#[test]
fn explicit_uncertainty_cannot_be_cached_as_a_known_rejection() {
    use rpi_node_api::api::realtime::{RealtimeError, RealtimeErrorCode};
    let registry = registry(8, 4096);
    let reference = reserve(&registry);
    let outcome = OperationOutcome::Failed {
        error: RealtimeError {
            code: RealtimeErrorCode::OutcomeUnknown,
            message: "delivery could not be confirmed".into(),
        },
    };
    assert_eq!(
        execute(&registry, &reference, Value::Null)
            .finish(outcome)
            .unwrap(),
        OperationStatus::OutcomeUnknown
    );
    assert_eq!(
        registry.status("principal", &reference).unwrap(),
        OperationStatus::OutcomeUnknown
    );
    assert!(matches!(
        registry
            .claim("principal", &reference, Value::Null)
            .unwrap(),
        OperationClaim::Existing(OperationStatus::OutcomeUnknown)
    ));
}
