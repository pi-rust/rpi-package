use super::*;
pub async fn verify(owner: &Service, source_id: &str, project_id: &str) {
    let source = owner
        .resource_saved_history(source_id.into(), ResourceHistoryRequest::default())
        .await
        .unwrap();
    let source_record = owner
        .resource_configuration(source_id)
        .await
        .unwrap()
        .unwrap();
    let historical = source
        .messages
        .iter()
        .find(|entry| entry["role"] == "assistant")
        .unwrap()["entryId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut destinations = Vec::new();
    for mode in ["current-fork", "clone", "historical-fork"] {
        let request = service::ForkRequest {
            entry_id: (mode == "historical-fork").then(|| historical.clone()),
            title: Some(mode.into()),
        };
        let reply = if mode == "clone" {
            service::clone_session(owner, source_id.into(), request).await
        } else {
            service::fork_session(owner, source_id.into(), request).await
        };
        assert_eq!(reply.kind, ReplyKind::Created, "{mode}: {}", reply.value);
        let id = reply.value["sessionId"].as_str().unwrap().to_owned();
        assert_ne!(id, source_id);
        assert!(!destinations.contains(&id));
        let record = owner.resource_configuration(&id).await.unwrap().unwrap();
        assert_eq!(record.revision, 1);
        assert_eq!(record.selection, source_record.selection);
        assert!(record.operations.is_empty());
        assert!(record.current_settings.is_none());
        let host = owner.live_session(&id).await.unwrap();
        let state = host.resource_state().await.unwrap();
        assert_eq!(state.binding.revision, 1);
        assert!(state.binding.operation_id.is_none());
        assert_eq!(state.binding.fingerprint, source.binding.fingerprint);
        assert_ne!(
            state.generation,
            source_record.observation.as_ref().unwrap().generation
        );
        assert_eq!(host.state().await.unwrap()["thinkingLevel"], "medium");
        let history = host
            .command("get_messages", json!({"limit":128}), Duration::from_secs(5))
            .await
            .unwrap()
            .to_string();
        assert!(history.contains("prior real A message"));
        if mode == "historical-fork" {
            assert!(!history.contains("after same ID transition"));
        } else {
            assert!(history.contains("after same ID transition"));
        }
        assert!(
            owner
                .resource_configuration(source_id)
                .await
                .unwrap()
                .unwrap()
                .current_settings
                .is_some(),
            "fork never consumes its offline source stop capture"
        );
        let unchanged = owner
            .resource_saved_history(source_id.into(), ResourceHistoryRequest::default())
            .await
            .unwrap();
        assert_eq!(unchanged.source, source.source);
        assert_eq!(unchanged.binding, source.binding);
        assert!(owner.live_session(source_id).await.is_none());
        destinations.push(id);
    }
    owner
        .release_idle_resources(project_id.into(), ResourceScope::Project)
        .await
        .unwrap();
    for id in destinations {
        let opened = service::open_session(owner, id.clone()).await;
        assert_eq!(opened.0, ReplyKind::Success, "{}", opened.1);
        let host = owner.live_session(&id).await.unwrap();
        let observation = host.resource_state().await.unwrap();
        assert_eq!(observation.binding.revision, 1);
        assert!(observation.binding.operation_id.is_none());
        assert!(observation.body["modelEffectiveTools"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(host.state().await.unwrap()["thinkingLevel"], "medium");
        assert!(host
            .command("get_messages", json!({"limit":128}), Duration::from_secs(5))
            .await
            .unwrap()
            .to_string()
            .contains("prior real A message"));
    }
    assert_eq!(
        owner
            .resource_saved_history(source_id.into(), ResourceHistoryRequest::default())
            .await
            .unwrap()
            .source,
        source.source
    );
    println!("native-owned current/historical fork and clone: fresh revision1 identities, source unchanged, settings/history preserved, ordinary configured reopen passed");
}
