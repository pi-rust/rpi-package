use super::*;
use rpi_node_api::api::{ProviderApi, ProviderModel};
fn store(root: &Path) -> Store {
    Store {
        root: Some(root.join("agent")),
        salt: [41; 32],
        gate: Arc::new(tokio::sync::Mutex::new(())),
    }
}
fn def() -> ProviderDefinition {
    ProviderDefinition {
        id: "isolated-gateway".into(),
        name: "Test gateway".into(),
        api: ProviderApi::OpenaiCompletions,
        base_url: "https://example.invalid/v1".into(),
        models: vec![ProviderModel {
            id: "test/model".into(),
            name: "Test".into(),
            reasoning: true,
            context_window: Some(32768),
            max_tokens: Some(2048),
        }],
    }
}
fn change(s: &Store, action: ProviderAction) -> Result<ProviderReceipt, WorkspaceError> {
    let expected_revision = snapshot(s)?.revision;
    mutate(
        s,
        ProviderMutation {
            expected_revision,
            action,
        },
    )
}
fn raw(s: &Store, name: &str) -> Value {
    document(read(s.root().unwrap(), name).unwrap().as_deref()).unwrap()
}
#[test]
fn read_is_lazy_and_crud_is_native_single_file_cas() {
    let temp = tempdir();
    let s = store(temp.path());
    let first = snapshot(&s).unwrap();
    assert_eq!(catalog(&first).providers.len(), 0);
    assert!(!s.root().unwrap().exists());
    let saved = change(&s, ProviderAction::Upsert { definition: def() }).unwrap();
    assert!(saved.requires_restart);
    assert_eq!(saved.catalog.providers[0].definition.as_ref(), Some(&def()));
    assert!(!s.root().unwrap().join("auth.json").exists());
    assert!(
        mutate(
            &s,
            ProviderMutation {
                expected_revision: first.revision,
                action: ProviderAction::RemoveConfig { id: def().id }
            }
        )
        .is_err()
    );
    let key = "!do-not-run $VAR ${SECRET} $(touch marker)";
    let keyed = change(
        &s,
        ProviderAction::SetKey {
            id: def().id,
            key: key.into(),
        },
    )
    .unwrap();
    assert_eq!(
        raw(&s, "auth.json")[&def().id]["key"],
        "$!do-not-run $$VAR $${SECRET} $$(touch marker)"
    );
    assert!(
        !serde_json::to_string(&keyed)
            .unwrap()
            .contains("do-not-run")
    );
    assert!(!format!("{keyed:?}").contains("do-not-run"));
    assert!(!temp.path().join("marker").exists());
    let removed = change(&s, ProviderAction::RemoveConfig { id: def().id }).unwrap();
    assert!(!removed.catalog.providers[0].configured);
    assert_eq!(
        removed.catalog.providers[0].stored_credential,
        StoredCredential::ApiKey
    );
    let cleared = change(
        &s,
        ProviderAction::ClearCredential {
            id: def().id,
            location: CredentialLocation::Stored,
        },
    )
    .unwrap();
    assert!(cleared.catalog.providers.is_empty());
}
#[test]
fn edits_preserve_unrelated_fields_order_and_do_not_resolve_headers() {
    let temp = tempdir();
    let s = store(temp.path());
    std::fs::create_dir_all(s.root().unwrap()).unwrap();
    let source = r#"{ // retain semantic values, never resolve commands
      "customRoot":17,"providers":{"z":{"api":"openai-completions","baseUrl":"https://example.invalid/v1","models":[]},
      "isolated-gateway":{"api":"openai-completions","baseUrl":"https://example.invalid/v1","apiKey":"secret-value","headers":{"Authorization":"!do-not-run"},"models":[{"id":"test/model","compat":{"requiresReasoningContentOnAssistantMessages":true},"headers":{"x":"secret-header"}}]},"a":{"models":[]}}}"#;
    std::fs::write(s.root().unwrap().join("models.json"), source).unwrap();
    let result = change(&s, ProviderAction::Upsert { definition: def() }).unwrap();
    let output = serde_json::to_string(&result).unwrap();
    assert!(!output.contains("secret-value"));
    assert!(!output.contains("secret-header"));
    assert!(!output.contains("do-not-run"));
    let m = raw(&s, "models.json");
    assert_eq!(m["customRoot"], 17);
    assert_eq!(
        m["providers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["z", "isolated-gateway", "a"]
    );
    assert_eq!(m["providers"][&def().id]["apiKey"], "secret-value");
    assert_eq!(
        m["providers"][&def().id]["headers"]["Authorization"],
        "!do-not-run"
    );
    assert_eq!(
        m["providers"][&def().id]["models"][0]["compat"]["requiresReasoningContentOnAssistantMessages"],
        true
    );
    change(
        &s,
        ProviderAction::ClearCredential {
            id: def().id,
            location: CredentialLocation::Config,
        },
    )
    .unwrap();
    assert!(
        raw(&s, "models.json")["providers"][&def().id]
            .get("apiKey")
            .is_none()
    );
}
#[test]
fn invalid_input_and_external_changes_preserve_all_bytes() {
    let temp = tempdir();
    let s = store(temp.path());
    for endpoint in [
        "file:///secret",
        "https://user:secret@example.invalid",
        "https://example.invalid?token=secret",
        "https://example.invalid/#secret",
        "https://example.invalid/\n",
    ] {
        let mut d = def();
        d.base_url = endpoint.into();
        assert!(change(&s, ProviderAction::Upsert { definition: d }).is_err());
    }
    let mut d = def();
    d.models.push(d.models[0].clone());
    assert!(change(&s, ProviderAction::Upsert { definition: d }).is_err());
    assert!(!s.root().unwrap().exists());
    change(&s, ProviderAction::Upsert { definition: def() }).unwrap();
    let before = snapshot(&s).unwrap();
    let path = s.root().unwrap().join("models.json");
    let bytes = b"{broken secret-value";
    std::fs::write(&path, bytes).unwrap();
    let error = mutate(
        &s,
        ProviderMutation {
            expected_revision: before.revision.clone(),
            action: ProviderAction::RemoveConfig { id: def().id },
        },
    )
    .unwrap_err();
    assert!(!error.to_string().contains("secret-value"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::write(&path, "{}").unwrap();
    assert!(
        mutate(
            &s,
            ProviderMutation {
                expected_revision: before.revision,
                action: ProviderAction::RemoveConfig { id: def().id }
            }
        )
        .is_err()
    );
}
#[test]
fn clearing_absent_source_never_invents_a_null_provider() {
    let temp = tempdir();
    let s = store(temp.path());
    change(
        &s,
        ProviderAction::ClearCredential {
            id: "absent".into(),
            location: CredentialLocation::Config,
        },
    )
    .unwrap();
    assert_eq!(raw(&s, "models.json"), json!({}));
    assert!(catalog(&snapshot(&s).unwrap()).providers.is_empty());
}
#[test]
fn revision_is_owner_salted_and_replaced_files_are_private() {
    let temp = tempdir();
    let s = store(temp.path());
    let mut second = store(temp.path());
    second.salt = [42; 32];
    assert_ne!(
        snapshot(&s).unwrap().revision,
        snapshot(&second).unwrap().revision
    );
    change(
        &s,
        ProviderAction::SetKey {
            id: "openai".into(),
            key: "test-only".into(),
        },
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(s.root().unwrap().join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        std::fs::read_dir(s.root().unwrap())
            .unwrap()
            .filter(|p| p
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
            .count(),
        0
    );
}
#[cfg(unix)]
#[test]
fn links_and_special_files_are_not_followed() {
    let temp = tempdir();
    let s = store(temp.path());
    std::fs::create_dir_all(s.root().unwrap()).unwrap();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, "{}").unwrap();
    std::os::unix::fs::symlink(&outside, s.root().unwrap().join("auth.json")).unwrap();
    assert!(snapshot(&s).is_err());
    assert!(
        change(
            &s,
            ProviderAction::SetKey {
                id: "openai".into(),
                key: "test".into()
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "{}");
}

#[test]
fn duplicate_json_keys_fail_closed_without_loss_or_secret_echo() {
    for source in [
        r#"{"providers":{},"providers":{"secret-id":{}}}"#,
        r#"{"provider":{"key":"private-value","key":"other"}}"#,
    ] {
        let error = document(Some(source.as_bytes())).err().unwrap();
        assert!(!error.to_string().contains("private-value"));
    }
    assert_eq!(
        document(Some(
            b"{\"providers\":{},// comment\n\"url\":\"https://example.invalid//quoted\"}"
        ))
        .unwrap()["url"],
        "https://example.invalid//quoted"
    );
}

#[test]
fn incompatible_optional_fields_are_readonly_and_never_overwritten() {
    let temp = tempdir();
    let s = store(temp.path());
    change(&s, ProviderAction::Upsert { definition: def() }).unwrap();
    for (field, value) in [
        ("contextWindow", json!("32768")),
        ("reasoning", json!("custom")),
        ("name", json!({"advanced":true})),
        ("maxTokens", json!(-1)),
    ] {
        let mut doc = raw(&s, "models.json");
        doc["providers"][&def().id]["models"][0][field] = value;
        let bytes = serde_json::to_vec(&doc).unwrap();
        let path = s.root().unwrap().join("models.json");
        std::fs::write(&path, &bytes).unwrap();
        let entry = catalog(&snapshot(&s).unwrap()).providers.remove(0);
        assert!(entry.definition.is_none());
        assert!(change(&s, ProviderAction::Upsert { definition: def() }).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, "{}").unwrap();
        change(&s, ProviderAction::Upsert { definition: def() }).unwrap();
    }
}
#[test]
fn unrelated_edits_preserve_absence_null_and_model_advanced_identity() {
    let temp = tempdir();
    let s = store(temp.path());
    std::fs::create_dir_all(s.root().unwrap()).unwrap();
    let original = json!({"providers":{def().id:{"api":"openai-completions","baseUrl":"https://example.invalid/v1","models":[{"id":"test/model","reasoning":null,"maxTokens":null,"compat":{"custom":true},"headers":{"x":"unchanged-secret"}}]}}});
    std::fs::write(
        s.root().unwrap().join("models.json"),
        serde_json::to_vec(&original).unwrap(),
    )
    .unwrap();
    let mut d = catalog(&snapshot(&s).unwrap()).providers[0]
        .definition
        .clone()
        .unwrap();
    d.name = "Only provider name changed".into();
    change(&s, ProviderAction::Upsert { definition: d }).unwrap();
    assert_eq!(
        raw(&s, "models.json")["providers"][&def().id]["models"],
        original["providers"][&def().id]["models"]
    );
}

fn tempdir() -> tempfile::TempDir {
    // macOS exposes its temp directory through /var, which is a system symlink.
    // Resolve that parent so explicit fixture links exercise the path checks.
    tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
}
