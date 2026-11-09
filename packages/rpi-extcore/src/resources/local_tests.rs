use super::*;
use serde_json::json;
fn profile() -> Value {
    json!({"version":1,"type":"resource_capabilities","loadedFacts":false,"exactSelection":{"versions":[1],"startupFlag":"--resource-config","stateCommand":"get_resource_state","mode":"rpc","maxConfigBytes":65536,"maxSnapshotBytes":262144,"toolModes":["default","only","none"]},"transition":{"version":1,"startupFlag":"--resource-transition","maxRequestBytes":65536},"management":{"version":1,"command":"resource","scopes":["project","global"],"kinds":["package","skill","extension"],"operations":["list","inspect","install","update","reinstall","remove","recover"],"explicitScopeRequired":true,"build":false,"network":false}})
}
fn list() -> LocalResourceRequest {
    LocalResourceRequest {
        project_id: "project".into(),
        scope: ResourceScope::Project,
        operation: LocalResourceOperation::List,
    }
}
#[test]
fn help_is_the_only_probe_for_old_cli_and_profiles_fail_closed() {
    let mut calls = Vec::new();
    let result = negotiate_with(|args, _| {
        calls.push(args.to_vec());
        Ok((true, b"old rpi help".to_vec(), vec![]))
    });
    assert_eq!(result.unwrap_err().kind, ResourceErrorKind::Unsupported);
    assert_eq!(calls, vec![vec![OsString::from("--help")]]);
    for raw in [
        b"not JSON".to_vec(),
        json!({"version":2,"type":"resource_capabilities","loadedFacts":false})
            .to_string()
            .into_bytes(),
        json!({"version":1,"type":"resource_capabilities","loadedFacts":true})
            .to_string()
            .into_bytes(),
    ] {
        let mut count = 0;
        assert!(negotiate_with(|args, _| {
            count += 1;
            assert_eq!(args.len(), 1);
            Ok((
                true,
                if count == 1 {
                    b"--resource-capabilities".to_vec()
                } else {
                    raw.clone()
                },
                vec![],
            ))
        })
        .is_err());
        assert_eq!(count, 2);
    }
    let caps = ResourceCapabilities { profile: profile() };
    assert!(caps.require_exact(true).is_ok());
    for pointer in [
        "/exactSelection/toolModes",
        "/exactSelection/maxConfigBytes",
        "/transition/maxRequestBytes",
    ] {
        let mut p = profile();
        *p.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(ResourceCapabilities { profile: p }
            .require_exact(true)
            .is_err());
    }
}
#[test]
fn legacy_package_inspect_preserves_safe_names_and_fixed_argv() {
    for scope in [ResourceScope::Project, ResourceScope::Global] {
        for name in [
            ".tools".into(),
            "_legacy".into(),
            format!(".{}", "x".repeat(95)),
        ] {
            let request = LocalResourceRequest {
                scope,
                operation: LocalResourceOperation::Inspect {
                    kind: ResourceKind::Package,
                    name: name.clone(),
                },
                ..list()
            };
            assert_eq!(
                arguments(&request).unwrap(),
                [
                    "resource",
                    "inspect",
                    "package",
                    &name,
                    "--scope",
                    scope.argument()
                ]
                .map(OsString::from)
            );
        }
    }
}
#[test]
fn legacy_package_inspect_rejects_unsafe_or_unbounded_names() {
    for name in [
        "",
        ".",
        "..",
        "...",
        ".tools.",
        "../escape",
        "..\\escape",
        ".tools/nested",
        ".tools\\nested",
        "-tools",
        "--help",
        ".bad:name",
        ".bad name",
        ".bad\0name",
        ".bad\nname",
        ".bad\tname",
        ".caf\u{e9}",
    ] {
        let request = LocalResourceRequest {
            operation: LocalResourceOperation::Inspect {
                kind: ResourceKind::Package,
                name: name.into(),
            },
            ..list()
        };
        assert_eq!(
            arguments(&request).unwrap_err().kind,
            ResourceErrorKind::InvalidInput,
            "{name:?}"
        );
    }
    let request = LocalResourceRequest {
        operation: LocalResourceOperation::Inspect {
            kind: ResourceKind::Package,
            name: format!(".{}", "x".repeat(96)),
        },
        ..list()
    };
    assert_eq!(
        arguments(&request).unwrap_err().kind,
        ResourceErrorKind::InvalidInput
    );
}
#[test]
fn legacy_dot_names_do_not_widen_other_inspection_or_managed_mutations() {
    for kind in [
        ResourceKind::Package,
        ResourceKind::Skill,
        ResourceKind::Extension,
    ] {
        let name = ".tools".to_owned();
        let source = std::env::temp_dir();
        let mut operations = vec![
            LocalResourceOperation::Install {
                kind,
                name: name.clone(),
                source: source.clone(),
                version: None,
            },
            LocalResourceOperation::Update {
                kind,
                name: name.clone(),
                source: source.clone(),
                version: None,
            },
            LocalResourceOperation::Reinstall {
                kind,
                name: name.clone(),
                source,
                version: None,
            },
            LocalResourceOperation::Remove {
                kind,
                name: name.clone(),
            },
        ];
        if !matches!(kind, ResourceKind::Package) {
            operations.push(LocalResourceOperation::Inspect { kind, name });
        }
        for operation in operations {
            let request = LocalResourceRequest {
                operation,
                ..list()
            };
            assert_eq!(
                arguments(&request).unwrap_err().kind,
                ResourceErrorKind::InvalidInput,
                "{:?}",
                request.operation
            );
        }
    }
}
#[test]
fn management_requires_kind_scope_operation_and_native_v1_input_bounds() {
    let request = list();
    let args = arguments(&request).unwrap();
    assert!(ResourceCapabilities { profile: profile() }
        .require_management(&request, &args)
        .is_ok());
    for key in ["scopes", "operations"] {
        let mut p = profile();
        p["management"][key] = json!([]);
        assert!(ResourceCapabilities { profile: p }
            .require_management(&request, &args)
            .is_err());
    }
    let mut request = LocalResourceRequest {
        operation: LocalResourceOperation::Inspect {
            kind: ResourceKind::Skill,
            name: "x".repeat(96),
        },
        ..list()
    };
    assert!(arguments(&request).is_ok());
    let mut p = profile();
    p["management"]["kinds"] = json!([]);
    assert!(ResourceCapabilities { profile: p }
        .require_management(&request, &arguments(&request).unwrap())
        .is_err());
    request.operation = LocalResourceOperation::Inspect {
        kind: ResourceKind::Skill,
        name: "x".repeat(97),
    };
    assert!(arguments(&request).is_err());
    for version in ["x".repeat(129), "-build".into(), "bad\nversion".into()] {
        request.operation = LocalResourceOperation::Install {
            kind: ResourceKind::Skill,
            name: "safe".into(),
            source: std::env::temp_dir(),
            version: Some(version),
        };
        assert!(arguments(&request).is_err());
    }
}
#[test]
fn success_context_is_strict_but_native_usage_failure_can_have_null_context() {
    let request = list();
    let args = arguments(&request).unwrap();
    let valid = json!({"version":1,"success":true,"operation":"list","scope":"project","kind":null,"name":null,"data":{"outcome":"observed"}});
    assert!(
        parse_envelope(
            true,
            &serde_json::to_vec(&valid).unwrap(),
            b"",
            &request,
            &args
        )
        .unwrap()
        .success
    );
    let failure = json!({"version":1,"success":false,"operation":null,"scope":null,"kind":null,"name":null,"error":"native usage diagnostic"});
    let parsed = parse_envelope(
        false,
        &serde_json::to_vec(&failure).unwrap(),
        b"",
        &request,
        &args,
    )
    .unwrap();
    assert!(!parsed.success);
    assert_eq!(parsed.observation["error"], "native usage diagnostic");
    for (key, value) in [
        ("scope", json!("global")),
        ("operation", json!("install")),
        ("kind", json!("package")),
        ("name", json!("other")),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert!(parse_envelope(
            true,
            &serde_json::to_vec(&invalid).unwrap(),
            b"",
            &request,
            &args
        )
        .is_err());
    }
    assert!(parse_envelope(
        false,
        &serde_json::to_vec(&valid).unwrap(),
        b"",
        &request,
        &args
    )
    .is_err());
}

#[test]
#[ignore = "subprocess fixture, invoked only by bounded capture tests"]
#[allow(clippy::zombie_processes)] // Deliberately orphaned descendant; ProcessTree must reap it.
fn cli_fixture() {
    use std::io::Write;
    let cwd = std::env::current_dir().unwrap();
    match std::fs::read_to_string(cwd.join("behavior.txt"))
        .unwrap()
        .as_str()
    {
        "overflow" => {
            let _ = std::io::stdout().write_all(&vec![b'x'; 128 * 1024]);
        }
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "holder" => {
            let sub = cwd.join("descendant");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("behavior.txt"), "sleep").unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(fixture_args())
                .current_dir(&sub)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            std::fs::write(cwd.join("descendant.pid"), child.id().to_string()).unwrap();
            // Descendant deliberately inherits the capture descriptors. The
            // native process owner, not this fixture, must retire its tree.
        }
        other => panic!("unknown fixture behavior {other}"),
    }
}
fn fixture_args() -> Vec<OsString> {
    [
        "--exact",
        "resources::local::tests::cli_fixture",
        "--ignored",
        "--nocapture",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}
#[test]
fn spool_capture_bounds_overflow_deadline_and_inherited_descriptor_lifetime() {
    let root = std::env::temp_dir().join(format!("resource-capture-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let bin = std::env::current_exe().unwrap();
    for behavior in ["overflow", "sleep", "holder"] {
        std::fs::write(root.join("behavior.txt"), behavior).unwrap();
        let start = Instant::now();
        let result = invoke(
            &bin,
            &root,
            &fixture_args(),
            8192,
            // Windows-hosted runners can spend more than 500 ms starting the
            // test binary under load. Keep the deadline bounded while allowing
            // process startup jitter; the outer cleanup assertion still caps
            // each fixture at five seconds.
            Duration::from_secs(2),
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{behavior} blocked cleanup"
        );
        match behavior {
            "overflow" => assert!(result.unwrap_err().message.contains("limit")),
            "sleep" => assert!(result.unwrap_err().message.contains("deadline")),
            "holder" => assert!(result.is_ok(), "{result:?}"),
            _ => unreachable!(),
        }
    }
    let spool = CaptureSpool::create().unwrap();
    let path = spool.path.clone();
    assert!(path.exists());
    drop(spool);
    assert!(!path.exists());
    // A Windows job can terminate the descendant before the kernel releases
    // its inherited directory handle. Retry only that sharing violation with a
    // bounded wait; other cleanup failures must still fail the test.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(error)
                if cfg!(windows)
                    && error.raw_os_error() == Some(32)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("remove test tree: {error}"),
        }
    }
}
