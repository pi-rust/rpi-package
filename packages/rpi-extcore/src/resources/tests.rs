use super::*;
use serde_json::json;
fn none() -> ResourceSelection {
    serde_json::from_value(json!({"version":1,"tools":{"mode":"none"}})).unwrap()
}
fn observed(id: &str, revision: u64, operation: Option<&str>) -> ResourceObservation {
    ResourceObservation::parse(json!({"version":1,"hostId":"generation-1","sessionId":id,"binding":{"revision":revision,"fingerprint":"a".repeat(64),"operationId":operation},"selection":none(),"loadedSkills":[],"loadedExtensions":[],"registeredTools":["read"],"modelEffectiveTools":[]})).unwrap()
}
fn applied() -> ResourceConfiguration {
    ResourceConfiguration {
        selection: none(),
        revision: 2,
        settings: Default::default(),
        current_settings: None,
        observation: Some(observed("session", 2, Some("one"))),
        operations: vec![ResourceOperation {
            request: ApplyResourcesRequest {
                expected_revision: 1,
                operation_id: "one".into(),
                target: none(),
                supersedes_operation_id: None,
            },
            transition: ResourceTransition {
                version: 1,
                operation_id: "one".into(),
                expected: ExpectedBinding {
                    revision: 1,
                    fingerprint: Some("b".repeat(64)),
                },
                target: none(),
            },
            restore_settings: None,
            status: ResourceOperationStatus::Applied,
            result_revision: Some(2),
            observation: Some(observed("session", 2, Some("one"))),
            error: None,
        }],
    }
}
#[test]
fn exact_selection_schema_distinguishes_none_empty_only_and_legacy_omission() {
    for value in [
        json!({"version":1,"tools":{"mode":"none","names":[]}}),
        json!({"version":1,"tools":{"mode":"default","names":[]}}),
        json!({"version":1,"tools":{"mode":"none"},"env":{}}),
        json!({"version":1,"tools":{"mode":"none"},"argv":[]}),
        json!({"version":1,"tools":{"mode":"none"},"executable":"rpi"}),
        json!({"version":1,"tools":{"mode":"none"},"credentials":{}}),
    ] {
        assert!(serde_json::from_value::<ResourceSelection>(value).is_err());
    }
    assert!(serde_json::from_str::<ResourceSelection>(
        r#"{"version":1,"version":1,"tools":{"mode":"none"}}"#
    )
    .is_err());
    let empty_only: ResourceSelection =
        serde_json::from_value(json!({"version":1,"tools":{"mode":"only","names":[]}})).unwrap();
    assert_ne!(empty_only, none());
    let legacy: crate::service::CreateSessionRequest = serde_json::from_value(json!({})).unwrap();
    assert!(legacy.resources.is_none());
    let exact: crate::service::CreateSessionRequest =
        serde_json::from_value(json!({"resources":{"version":1,"tools":{"mode":"none"}}})).unwrap();
    assert_eq!(exact.resources, Some(none()));
}
#[test]
fn bounded_observation_cannot_authorize_widened_tools_or_changed_identity() {
    let observation = observed("session", 1, None);
    observation.verify("session", &none()).unwrap();
    assert!(observation.verify("other", &none()).is_err());
    let mut body = observation.body.clone();
    body["modelEffectiveTools"] = json!(["read"]);
    assert!(ResourceObservation::parse(body)
        .unwrap()
        .verify("session", &none())
        .is_err());
    let mut body = observation.body.clone();
    body["oversized"] = json!("x".repeat(MAX_STATE_BYTES));
    assert_eq!(
        ResourceObservation::parse(body).unwrap_err().kind,
        ResourceErrorKind::LimitExceeded
    );
    for (field, value) in [
        ("hostId", json!(null)),
        ("version", json!(2)),
        (
            "binding",
            json!({"revision":"1","fingerprint":"a".repeat(64)}),
        ),
        ("registeredTools", json!(null)),
    ] {
        let mut body = observation.body.clone();
        body[field] = value;
        assert!(ResourceObservation::parse(body).is_err());
    }
}
#[test]
fn resource_record_rejects_corrupt_identity_status_revision_and_overflow() {
    let good = applied();
    good.validate_session("session").unwrap();
    assert!(good.validate_session("other").is_err());
    let value = serde_json::to_value(&good).unwrap();
    for (pointer, new) in [
        ("/revision", json!(3)),
        ("/operations/0/request/operation_id", json!("different")),
        ("/operations/0/transition/expected/fingerprint", json!(null)),
        ("/operations/0/observation/binding/revision", json!(3)),
        ("/operations/0/status", json!("pending")),
        ("/operations/0/result_revision", json!(1)),
        ("/operations/0/observation/generation", json!("forged")),
        ("/settings/model", json!("x".repeat(4097))),
    ] {
        let mut damaged = value.clone();
        *damaged.pointer_mut(pointer).unwrap() = new;
        assert!(
            serde_json::from_value::<ResourceConfiguration>(damaged)
                .unwrap()
                .validate()
                .is_err(),
            "{pointer}"
        );
    }
    let mut duplicate = good.clone();
    duplicate.operations.push(good.operations[0].clone());
    assert!(duplicate.validate().is_err());
    let mut oversized = good.clone();
    oversized.operations = vec![good.operations[0].clone(); MAX_OPERATIONS + 1];
    assert_eq!(
        oversized.validate().unwrap_err().kind,
        ResourceErrorKind::LimitExceeded
    );
    for id in ["", "has space", "../invalid", "é"] {
        assert!(validate_operation_id(id).is_err());
    }
    assert!(validate_operation_id(&"a".repeat(128)).is_ok());
    assert!(validate_operation_id(&"a".repeat(129)).is_err());
}
#[test]
fn registry_resource_write_failure_rolls_back_memory_and_corrupt_load_never_repairs() {
    use crate::registry::{Registry, RegistryEntry};
    let root =
        std::env::temp_dir().join(format!("resource-registry-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("registry.json");
    let mut registry = Registry::load(path.clone()).unwrap();
    let entry = RegistryEntry {
        session_id: "session".into(),
        project_id: None,
        title: None,
        parent_session_id: None,
        fork_entry_id: None,
        relation: None,
        created_at: 0,
        updated_at: 0,
    };
    registry
        .set_resource_configuration(entry.clone(), applied())
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
    let mut next = applied();
    next.settings.thinking = Some("off".into());
    assert!(registry.set_resource_configuration(entry, next).is_err());
    assert!(registry
        .resource_configuration("session")
        .unwrap()
        .settings
        .thinking
        .is_none());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_dir(path.with_extension("json.tmp")).unwrap();
    let mut corrupt: Value = serde_json::from_slice(&before).unwrap();
    corrupt["resources"]["session"]["revision"] = json!("2");
    let raw = serde_json::to_vec(&corrupt).unwrap();
    std::fs::write(&path, &raw).unwrap();
    assert!(Registry::load(path.clone()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), raw);
    let record = serde_json::to_string(&applied()).unwrap();
    let duplicate=format!("{{\"sessions\":[{{\"sessionId\":\"session\"}}],\"resources\":{{\"session\":{record},\"session\":{record}}}}}");
    std::fs::write(&path, duplicate).unwrap();
    assert!(Registry::load(path).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
