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

    // Preserve all offline-source assertions above before exercising a genuinely
    // live configured source. No release/stop capture precedes either child.
    let opened = service::open_session(owner, source_id.into()).await;
    assert_eq!(opened.0, ReplyKind::Success, "{}", opened.1);
    let live_source_record = owner
        .resource_configuration(source_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        live_source_record.current_settings.is_none(),
        "opening the source must consume its owned-stop capture"
    );
    let live_source = owner.live_session(source_id).await.unwrap();
    live_source.set_model("alt", Some("fixture")).await.unwrap();
    live_source.set_thinking_level("high").await.unwrap();
    let live_settings = live_source.state().await.unwrap();
    assert_eq!(live_settings["modelId"], "alt");
    assert_eq!(live_settings["provider"], "fixture");
    assert_eq!(live_settings["thinkingLevel"], "high");
    let live_observation = live_source.resource_state().await.unwrap();
    assert_eq!(
        live_observation.generation,
        live_source_record.observation.as_ref().unwrap().generation
    );
    let mut live_destinations = Vec::new();
    for mode in ["live-fork", "live-clone"] {
        let request = service::ForkRequest {
            entry_id: None,
            title: Some(mode.into()),
        };
        let reply = if mode == "live-clone" {
            service::clone_session(owner, source_id.into(), request).await
        } else {
            service::fork_session(owner, source_id.into(), request).await
        };
        assert_eq!(reply.kind, ReplyKind::Created, "{mode}: {}", reply.value);
        let id = reply.value["sessionId"].as_str().unwrap().to_owned();
        assert_ne!(id, source_id);
        assert!(!live_destinations.contains(&id));
        let record = owner.resource_configuration(&id).await.unwrap().unwrap();
        assert_eq!(record.revision, 1);
        assert_eq!(record.selection, live_source_record.selection);
        assert!(record.operations.is_empty());
        assert!(record.current_settings.is_none());
        let host = owner.live_session(&id).await.unwrap();
        let actual = host.state().await.unwrap();
        assert_eq!(actual["modelId"], "alt", "{mode}: {actual}");
        assert_eq!(actual["provider"], "fixture", "{mode}: {actual}");
        assert_eq!(actual["thinkingLevel"], "high", "{mode}: {actual}");
        let observation = host.resource_state().await.unwrap();
        assert_eq!(observation.session_id, id);
        assert_eq!(observation.binding.revision, 1);
        assert!(observation.binding.operation_id.is_none());
        assert_eq!(
            observation.binding.fingerprint,
            live_observation.binding.fingerprint
        );
        assert_eq!(
            serde_json::from_value::<ResourceSelection>(observation.body["selection"].clone())
                .unwrap(),
            live_source_record.selection
        );
        assert_ne!(observation.generation, live_observation.generation);
        assert!(Arc::ptr_eq(
            &owner.live_session(source_id).await.unwrap(),
            &live_source
        ));
        let unchanged_settings = live_source.state().await.unwrap();
        for field in ["modelId", "provider", "thinkingLevel"] {
            assert_eq!(
                unchanged_settings[field], live_settings[field],
                "{mode}: source {field}"
            );
        }
        let unchanged_observation = live_source.resource_state().await.unwrap();
        assert_eq!(
            unchanged_observation.generation,
            live_observation.generation
        );
        assert_eq!(unchanged_observation.binding, live_observation.binding);
        assert_eq!(
            unchanged_observation.body["selection"],
            live_observation.body["selection"]
        );
        assert_eq!(
            serde_json::to_value(
                owner
                    .resource_configuration(source_id)
                    .await
                    .unwrap()
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&live_source_record).unwrap(),
            "live fork/clone must not rewrite source resource authority"
        );
        println!("{mode}: native modelId=alt provider=fixture thinkingLevel=high; fresh revision1 exact destination; source generation/settings unchanged");
        live_destinations.push(id);
    }
}
