use rpi_components::{host::Host, Action, Bundle, Call, InstallRequest, Manifest, Panel};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
fn bundle(id: &str) -> Bundle {
    Bundle::new(
        Manifest {
            schema_version: 1,
            runtime: None,
            id: id.into(),
            version: "1.0.0".into(),
            title: "Custom".into(),
            description: "Independent UI".into(),
            panels: vec![Panel {
                id: "settings".into(),
                title: "Settings".into(),
                slot: "settings".into(),
                entry: "index.html".into(),
                protocol: 1,
            }],
            actions: BTreeMap::from([(
                "run".into(),
                Action {
                    capability: "test.run".into(),
                    local_only: false,
                },
            )]),
        },
        BTreeMap::from([("index.html".into(), "<h1>Custom interface</h1>".into())]),
    )
    .unwrap()
}
fn call() -> Call {
    Call {
        action: "run".into(),
        arguments: json!({}),
    }
}
#[test]
fn packages_persist_without_replacing_bundled_components() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::open(root.path().into()).unwrap();
    host.register_builtin(bundle("official")).unwrap();
    assert!(host
        .install(InstallRequest {
            bundle: bundle("official"),
            grants: BTreeSet::new()
        })
        .is_err());
    assert!(host.remove("official").is_err());
    assert!(host.remove("../outside").is_err());
    host.install(InstallRequest {
        bundle: bundle("downloaded"),
        grants: BTreeSet::new(),
    })
    .unwrap();
    drop(host);
    let host = Host::open(root.path().into()).unwrap();
    assert_eq!(host.catalog().unwrap().len(), 1);
    assert!(host
        .panel("downloaded", "settings")
        .unwrap()
        .contains("Custom interface"));
    host.remove("downloaded").unwrap();
    assert!(host.catalog().unwrap().is_empty());
}
#[test]
fn corrupt_or_ungranted_packages_never_execute() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::open(root.path().into()).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    host.register_capability(
        "test.run",
        true,
        Arc::new(move |_, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(json!("ok"))
        }),
    )
    .unwrap();
    let mut corrupt = bundle("corrupt");
    corrupt.files.insert("index.html".into(), "changed".into());
    assert!(host
        .install(InstallRequest {
            bundle: corrupt,
            grants: BTreeSet::new()
        })
        .is_err());
    let mut traversal = bundle("bad");
    traversal.files.insert("../escape.html".into(), "x".into());
    assert!(traversal.validate().is_err());
    host.install(InstallRequest {
        bundle: bundle("ungranted"),
        grants: BTreeSet::new(),
    })
    .unwrap();
    assert!(host.call("ungranted", call(), true).is_err());
    host.install(InstallRequest {
        bundle: bundle("granted"),
        grants: BTreeSet::from(["test.run".into()]),
    })
    .unwrap();
    // Host policy is authoritative even if a package claims remote-safe actions.
    assert!(host.call("granted", call(), false).is_err());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(host.call("granted", call(), true).unwrap(), json!("ok"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
