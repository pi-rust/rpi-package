use super::*;
#[tokio::test]
async fn caller_cancellation_alone_retains_fork_owner_source_fence_and_capacity() {
    let f = Fixture::new(false);
    f.open().await;
    f.flag("pause-readback");
    let source = f.owner.live_session("source").await.unwrap();
    let owner = f.owner.clone();
    let caller = tokio::spawn(async move {
        service::clone_session(&owner, "source".into(), ForkRequest::default()).await
    });
    wait_file(&f.root.join("readback-paused")).await;
    let id = std::fs::read_to_string(f.root.join("readback-paused")).unwrap();
    caller.abort();
    let _ = caller.await;
    assert!(
        source.try_run_lease().is_err(),
        "retained fork owns the source fence after caller loss"
    );
    // A second bounded owner waits for lifecycle; a third must refuse admission.
    let owner = f.owner.clone();
    let waiting = tokio::spawn(async move {
        service::clone_session(&owner, "source".into(), ForkRequest::default()).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let capacity = service::clone_session(&f.owner, "source".into(), ForkRequest::default()).await;
    assert_eq!(capacity.kind, ReplyKind::Conflict);
    assert!(capacity.value.to_string().contains("capacity"));
    std::fs::remove_file(f.root.join("pause-readback")).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.owner.live_session(&id).await.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(waiting.await.unwrap().kind, ReplyKind::Created);
    let disk = f.disk();
    let record = disk.resource_configuration(&id).unwrap();
    assert_eq!(record.selection, selection());
    assert_eq!(record.revision, 1);
    assert!(record.operations.is_empty());
    assert!(source.try_run_lease().is_ok());
    f.end().await;
    assert!(f.root.join(format!("stopped-{id}")).exists());
}
#[tokio::test]
async fn late_metadata_failure_retains_capture_and_latches_until_owner_restart() {
    let f = Fixture::new(true);
    f.flag("fail-metadata");
    let first = service::open_session(&f.owner, "source".into()).await;
    assert_eq!(first.0, ReplyKind::Internal, "{}", first.1);
    assert!(f
        .disk()
        .resource_configuration("source")
        .unwrap()
        .current_settings
        .is_some());
    assert!(f.owner.live_session("source").await.is_none());
    assert!(f.root.join("stopped-source").exists());
    std::fs::remove_file(f.root.join("fail-metadata")).unwrap();
    std::fs::remove_dir(f.root.join("registry.json.tmp")).unwrap();
    assert_eq!(
        service::open_session(&f.owner, "source".into()).await.0,
        ReplyKind::Internal
    );
    f.end().await;
    let owner = Service::new(f.owner.config.clone()).unwrap();
    let opened = service::open_session(&owner, "source".into()).await;
    assert_eq!(opened.0, ReplyKind::Success, "{}", opened.1);
    let actual = state(&owner, "source").await;
    let consumed = owner
        .resource_configuration("source")
        .await
        .unwrap()
        .unwrap()
        .current_settings
        .is_none();
    owner.shutdown().await;
    assert_eq!(actual["model"], "saved-model");
    assert_eq!(actual["provider"], "saved-provider");
    assert_eq!(actual["thinkingLevel"], "high");
    assert!(consumed);
}
#[tokio::test]
async fn cancelled_open_retains_capture_until_full_attachment_publication() {
    let f = Fixture::new(true);
    f.flag("pause-branch");
    let owner = f.owner.clone();
    let caller = tokio::spawn(async move { service::open_session(&owner, "source".into()).await });
    wait_file(&f.root.join("branch-paused")).await;
    caller.abort();
    let _ = caller.await;
    let retained = f
        .disk()
        .resource_configuration("source")
        .unwrap()
        .current_settings
        .is_some();
    f.flag("fail-branch");
    std::fs::remove_file(f.root.join("pause-branch")).unwrap();
    wait_file(&f.root.join("stopped-source")).await;
    std::fs::remove_file(f.root.join("fail-branch")).unwrap();
    f.open().await;
    let actual = state(&f.owner, "source").await;
    f.end().await;
    assert!(retained);
    assert_eq!(actual["model"], "saved-model");
    assert_eq!(actual["thinkingLevel"], "high");
}
#[tokio::test]
async fn failed_fork_navigation_attachment_and_publication_clean_candidates() {
    for failure in ["fail-navigation", "fail-branch", "fail-metadata"] {
        let f = Fixture::new(true);
        f.flag(failure);
        let reply = service::fork_session(
            &f.owner,
            "source".into(),
            ForkRequest {
                entry_id: Some("historical-entry".into()),
                title: None,
            },
        )
        .await;
        f.end().await;
        assert_ne!(reply.kind, ReplyKind::Created, "{failure}: {}", reply.value);
        assert_eq!(f.disk().entries().len(), 1, "{failure}");
        assert!(f
            .disk()
            .resource_configuration("source")
            .unwrap()
            .current_settings
            .is_some());
        for file in std::fs::read_dir(&f.root).unwrap().filter_map(Result::ok) {
            let name = file.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_prefix("started-") {
                assert!(
                    f.root.join(format!("stopped-{id}")).exists(),
                    "{failure}: cleanup {id}"
                );
            }
        }
    }
}
#[tokio::test]
async fn idle_release_capture_is_retried_without_replaying_historical_settings() {
    let f = Fixture::new(false);
    f.open().await;
    let host = f.owner.live_session("source").await.unwrap();
    host.set_model("released-model", Some("released-provider"))
        .await
        .unwrap();
    host.set_thinking_level("high").await.unwrap();
    let projects = service::list_projects(&f.owner).await;
    let project = projects["projects"][0]["id"].as_str().unwrap();
    f.owner
        .release_idle_resources(project.into(), ResourceScope::Project)
        .await
        .unwrap();
    assert!(f
        .disk()
        .resource_configuration("source")
        .unwrap()
        .current_settings
        .is_some());
    f.flag("fail-branch");
    assert_eq!(
        service::open_session(&f.owner, "source".into()).await.0,
        ReplyKind::HostUnavailable
    );
    std::fs::remove_file(f.root.join("fail-branch")).unwrap();
    f.open().await;
    let actual = state(&f.owner, "source").await;
    f.end().await;
    assert_eq!(actual["model"], "released-model");
    assert_eq!(actual["provider"], "released-provider");
    assert_eq!(actual["thinkingLevel"], "high");
}
#[tokio::test]
async fn legacy_offline_fork_api_keeps_legacy_authority() {
    let f = Fixture::new(false);
    let mut registry = f.disk();
    registry.remove("source").unwrap();
    registry.upsert(entry("legacy")).unwrap();
    let owner = Service::new(f.owner.config.clone()).unwrap();
    let reply = service::clone_session(&owner, "legacy".into(), ForkRequest::default()).await;
    assert_eq!(reply.kind, ReplyKind::Created, "{}", reply.value);
    let id = reply.value["sessionId"].as_str().unwrap();
    assert!(owner.resource_configuration(id).await.unwrap().is_none());
    assert_eq!(
        owner.live_session(id).await.unwrap().ready()["resourceState"]["enabled"],
        false
    );
    let opened = service::open_session(&owner, "legacy".into()).await;
    assert_eq!(opened.0, ReplyKind::Success, "{}", opened.1);
    let host = owner.live_session("legacy").await.unwrap();
    let run = host.try_run_lease().unwrap();
    let live = service::clone_session(&owner, "legacy".into(), ForkRequest::default()).await;
    drop(run);
    assert_eq!(
        live.kind,
        ReplyKind::Created,
        "legacy live runs retain the existing fork API: {}",
        live.value
    );
    assert!(owner
        .resource_configuration(live.value["sessionId"].as_str().unwrap())
        .await
        .unwrap()
        .is_none());
    owner.shutdown().await;
    f.end().await;
}
#[test]
fn configuration_publication_preserves_existing_metadata_not_placeholder_fields() {
    let f = Fixture::new(false);
    let mut registry = f.disk();
    let mut metadata = entry("source");
    metadata.title = Some("renamed".into());
    metadata.project_id = Some("existing-project".into());
    metadata.parent_session_id = Some("parent".into());
    metadata.fork_entry_id = Some("entry".into());
    metadata.relation = Some("fork".into());
    metadata.updated_at = 40;
    registry.upsert(metadata).unwrap();
    let mut placeholder = entry("source");
    placeholder.project_id = Some("other-project".into());
    placeholder.updated_at = 50;
    registry
        .set_resource_configuration(placeholder, record())
        .unwrap();
    let actual = registry.get("source").unwrap();
    assert_eq!(actual.title.as_deref(), Some("renamed"));
    assert_eq!(actual.project_id.as_deref(), Some("existing-project"));
    assert_eq!(actual.parent_session_id.as_deref(), Some("parent"));
    assert_eq!(actual.fork_entry_id.as_deref(), Some("entry"));
    assert_eq!(actual.relation.as_deref(), Some("fork"));
    assert_eq!(actual.updated_at, 50);
}
