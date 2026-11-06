use super::{NativeBinding, ResourceObservation, immutable, json, observation_matches};
#[test]
fn fresh_native_null_operation_matches_typed_omitted_optional_binding() {
    let body = json!({"hostId":"real-generation","binding":{"revision":1,"fingerprint":"a".repeat(64),"operationId":null}});
    let binding: NativeBinding = serde_json::from_value(body["binding"].clone()).unwrap();
    assert!(
        serde_json::to_value(&binding)
            .unwrap()
            .get("operationId")
            .is_none()
    );
    let saved = ResourceObservation {
        generation: "real-generation".into(),
        session_id: "same-id".into(),
        binding,
        body: body.clone(),
    };
    assert!(observation_matches(&body, &saved));
    let mut old = body.clone();
    old["hostId"] = json!("old-generation");
    assert!(!observation_matches(&old, &saved));
    let mut stale = body.clone();
    stale["binding"]["revision"] = json!(2);
    assert!(!observation_matches(&stale, &saved));
    let mut invalid = body;
    invalid["binding"]["revision"] = json!("1");
    assert!(!observation_matches(&invalid, &saved));
}
#[test]
fn immutable_identity_never_overwrites_and_bounds_inputs() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deep/file");
    immutable(&path, b"original").unwrap();
    immutable(&path, b"original").unwrap();
    assert!(immutable(&path, b"different").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    assert!(
        immutable(
            &root.path().join("too-large"),
            &vec![0; 8 * 1024 * 1024 + 1]
        )
        .is_err()
    );
    assert!(!root.path().join("too-large").exists());
}
#[test]
fn symlink_or_junction_ancestor_cannot_escape_source_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = root.path().join("escape");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    #[cfg(windows)]
    {
        assert!(
            std::process::Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(outside.path())
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    assert!(immutable(&link.join("should-not-exist"), b"escape").is_err());
    assert!(!outside.path().join("should-not-exist").exists());
    #[cfg(windows)]
    std::fs::remove_dir(link).unwrap();
}
