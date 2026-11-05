use super::*;
#[test]
fn supersede_preserves_immutable_ids_payload_and_native_chain() {
    let target: ResourceSelection =
        serde_json::from_value(json!({"version":1,"tools":{"mode":"none"}})).unwrap();
    let first = ApplyResourcesRequest {
        expected_revision: 0,
        operation_id: "typo".into(),
        target: target.clone(),
        supersedes_operation_id: None,
    };
    let expected = ExpectedBinding {
        revision: 0,
        fingerprint: None,
    };
    let pending = planned_record(None, &first, expected.clone(), None, None).unwrap();
    let second = ApplyResourcesRequest {
        operation_id: "corrected".into(),
        supersedes_operation_id: Some("typo".into()),
        ..first.clone()
    };
    let corrected =
        planned_record(Some(&pending), &second, expected.clone(), None, Some(0)).unwrap();
    assert_eq!(corrected.revision, 0);
    assert!(corrected.blocked());
    assert_eq!(
        corrected.operations[0].status,
        ResourceOperationStatus::Superseded
    );
    assert_eq!(corrected.operations[0].request, first);
    assert_eq!(
        corrected.operations[0].transition,
        pending.operations[0].transition
    );
    corrected.validate().unwrap();
    let value = serde_json::to_value(&corrected).unwrap();
    for (pointer, value2) in [
        ("/operations/0/status", json!("blocked")),
        (
            "/operations/1/request/supersedes_operation_id",
            json!("wrong"),
        ),
        ("/operations/1/transition/expected/revision", json!(1)),
        ("/operations/0/result_revision", json!(1)),
        ("/operations/1/request/operation_id", json!("typo")),
        ("/operations/1/request/expected_revision", json!(1)),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = value2;
        assert!(
            serde_json::from_value::<ResourceConfiguration>(bad)
                .unwrap()
                .validate()
                .is_err(),
            "{pointer}"
        );
    }
    let mut missing = corrected.clone();
    missing.operations.pop();
    assert!(missing.validate().is_err());
    let mut no_association = second;
    no_association.supersedes_operation_id = None;
    assert!(planned_record(Some(&pending), &no_association, expected, None, Some(0)).is_err());
}
#[tokio::test]
async fn legacy_failed_intent_has_no_fake_configuration_but_ledger_survives_restart() {
    let root = std::env::temp_dir().join(format!("resource-intent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let config = BackendConfig {
        project: root.clone(),
        registry: root.join("registry.json"),
        ..BackendConfig::default()
    };
    let owner = Service::new(config.clone()).unwrap();
    let project = project_by_id(&owner, &owner.default_project_id)
        .await
        .unwrap();
    let first:ApplyResourcesRequest=serde_json::from_value(json!({"expected_revision":0,"operation_id":"bad","target":{"version":1,"tools":{"mode":"only","names":["typo"]}}})).unwrap();
    let mut blocked = planned_record(
        None,
        &first,
        ExpectedBinding {
            revision: 0,
            fingerprint: None,
        },
        None,
        None,
    )
    .unwrap();
    blocked.operations[0].status = ResourceOperationStatus::Blocked;
    owner
        .registry
        .write()
        .await
        .set_resource_configuration(entry("legacy", &project), blocked.clone())
        .unwrap();
    assert!(owner
        .resource_configuration("legacy")
        .await
        .unwrap()
        .is_none());
    assert_eq!(owner.resource_operations("legacy").await.unwrap().len(), 1);
    let second:ApplyResourcesRequest=serde_json::from_value(json!({"expected_revision":0,"operation_id":"fixed","supersedes_operation_id":"bad","target":{"version":1,"tools":{"mode":"none"}}})).unwrap();
    let next = planned_record(
        Some(&blocked),
        &second,
        ExpectedBinding {
            revision: 0,
            fingerprint: None,
        },
        None,
        Some(0),
    )
    .unwrap();
    let staged_path = root.join("next.json");
    let mut staged = Registry::load(staged_path.clone()).unwrap();
    staged
        .set_resource_configuration(entry("legacy", &project), next.clone())
        .unwrap();
    let next_bytes = std::fs::read(staged_path).unwrap();
    for fault in 0..=2 {
        let path = root.join(format!("{fault}.json"));
        let mut registry = Registry::load(path.clone()).unwrap();
        registry
            .set_resource_configuration(entry("legacy", &project), blocked.clone())
            .unwrap();
        assert!(crate::registry::atomic_write_resource_test(&path, &next_bytes, fault).is_err());
        let disk = Registry::load(path).unwrap();
        let disk = disk.resource_configuration("legacy").unwrap();
        assert!(disk.operations.len() == 1 || disk.operations.len() == 2);
        if disk.operations.len() == 2 {
            assert_eq!(
                disk.operations[0].status,
                ResourceOperationStatus::Superseded
            );
            assert_eq!(disk.operations[1].request, second);
        } else {
            assert_eq!(disk.operations[0].status, ResourceOperationStatus::Blocked);
        }
    }
    owner.shutdown().await;
    drop(owner);
    let restart = Service::new(config).unwrap();
    assert!(restart
        .resource_configuration("legacy")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        restart
            .resource_operation("legacy", "bad")
            .await
            .unwrap()
            .unwrap()
            .request,
        first
    );
    restart.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}
