use super::{
    NativeReadiness, NativeReleaseBlock, NativeReleaseReport, NativeSelection,
    NativeSessionResources, NativeTools,
};
use serde_json::json;
#[test]
fn stable_resource_contract_retains_unresolved_identity_and_unknown_readiness() {
    let raw = json!({"configuration":{"revision":7,"desired":{"mainPackage":{"reference":null,"identity":"missing-manager-package","diagnostic":"source removed"},"packageInstructions":["prompts/system.md"],"skills":[],"extensions":[],"tools":{"mode":"none"},"supplementaryInstructions":null},"historicalObservation":{"hostId":"old"}},"operations":[],"readiness":"unknown","executable":null,"actual":null,"generation":null,"diagnostic":"fresh native state unavailable"});
    let state: NativeSessionResources = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(state.readiness, NativeReadiness::Unknown);
    assert_eq!(state.executable, None);
    assert_eq!(
        state
            .configuration
            .as_ref()
            .unwrap()
            .desired
            .main_package
            .as_ref()
            .unwrap()
            .identity,
        "missing-manager-package"
    );
    assert_eq!(serde_json::to_value(state).unwrap(), raw);
}
#[test]
fn exact_request_rejects_executable_or_argv_authority_and_distinguishes_none() {
    let raw = json!({"mainPackage":"opaque","skills":[],"extensions":[],"packageInstructions":[],"tools":{"mode":"none"},"supplementaryInstructions":"text"});
    let selection: NativeSelection = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(selection.tools, NativeTools::None {});
    for field in ["dll", "argv", "executable", "env", "instructions"] {
        let mut unsafe_request = raw.clone();
        unsafe_request[field] = json!("client-controlled");
        assert!(
            serde_json::from_value::<NativeSelection>(unsafe_request).is_err(),
            "{field}"
        );
    }
    let only: NativeTools =
        serde_json::from_value(json!({"mode":"only","names":["read"]})).unwrap();
    assert_ne!(only, selection.tools);
    assert!(serde_json::from_value::<NativeTools>(json!({"mode":"none","names":[]})).is_err());
}
#[test]
fn partial_owner_release_has_named_success_and_blocked_results() {
    let report = NativeReleaseReport {
        released: vec!["idle-a".into()],
        blocked: vec![NativeReleaseBlock {
            project_id: "busy-b".into(),
            diagnostic: "active run".into(),
        }],
    };
    let raw = serde_json::to_value(report).unwrap();
    assert_eq!(raw["released"][0], "idle-a");
    assert_eq!(raw["blocked"][0]["projectId"], "busy-b");
}
