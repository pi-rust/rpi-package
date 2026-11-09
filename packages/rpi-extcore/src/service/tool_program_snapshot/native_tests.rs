//! Opt-in real-native, read-only recovery evidence; never uses PATH or a model.
use super::*;
use crate::{host::SessionOptions, service::BackendConfig};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

fn digest(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    format!("{:x}", hash.finalize())
}
fn path_env(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).expect(name));
    assert!(
        path.is_absolute(),
        "{name} must be explicit absolute trusted path"
    );
    path
}
fn primary_files(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink());
        let path = entry.path();
        if kind.is_dir() {
            primary_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl")
            && !path.to_string_lossy().ends_with(".progress.jsonl")
        {
            out.push(path);
        }
    }
}

#[tokio::test]
#[ignore = "requires explicit trusted frozen native binary, SHA and retained smoke evidence; no PATH or model"]
async fn actual_native_readonly_child_snapshots() {
    let executable = path_env("RPI_CODEMODE_NATIVE_BIN");
    let expected_hash =
        std::env::var("RPI_CODEMODE_NATIVE_SHA256").expect("explicit native SHA256 required");
    assert_eq!(digest(&executable), expected_hash);
    let source = path_env("RPI_CODEMODE_NATIVE_EVIDENCE");
    let output = path_env("RPI_CODEMODE_CORE_EVIDENCE_OUT");
    fs::create_dir(&output).unwrap();
    // Compile a std-only fixed-flag launcher, not a mock native peer. HostSession
    // owns its process tree; both pipes go directly to the frozen real native.
    let launcher = output.join(if cfg!(windows) {
        "native-launcher.exe"
    } else {
        "native-launcher"
    });
    let source_file = output.join("native-launcher.rs");
    fs::write(&source_file, include_str!("native_launcher.rs")).unwrap();
    assert!(std::process::Command::new("rustc")
        .args(["--edition", "2021"])
        .arg(&source_file)
        .arg("-o")
        .arg(&launcher)
        .status()
        .unwrap()
        .success());
    let mut sessions = Vec::new();
    for entry in fs::read_dir(&source).unwrap() {
        let sessions_root = entry.unwrap().path().join("sessions");
        if sessions_root.is_dir() {
            primary_files(&sessions_root, &mut sessions);
        }
    }
    assert!(!sessions.is_empty() && sessions.len() <= 32);
    sessions.sort();
    let mut results = Vec::new();
    let mut selected_children = 0;
    let mut execution_events = 0;
    for (n, original) in sessions.iter().enumerate() {
        let original_hash = digest(original);
        let mut reader = BufReader::new(fs::File::open(original).unwrap());
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        assert!(header.len() <= 65536);
        let header: Value = serde_json::from_str(&header).unwrap();
        let id = header["id"]
            .as_str()
            .expect("native session header ID")
            .to_owned();
        let mut expected = HashMap::<String, Vec<ChildPhase>>::new();
        for line in reader.lines() {
            let entry: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if entry["customType"] == "rpi.tool_program.child_phase.v1" {
                let mut phase = entry["data"].clone();
                phase["entryId"] = entry["id"].clone();
                phase["seq"] = entry["seq"].clone();
                let phase: ChildPhase = serde_json::from_value(phase).unwrap();
                expected
                    .entry(phase.child_id.clone())
                    .or_default()
                    .push(phase);
            }
        }
        let case = output.join(format!("session-{n}"));
        fs::create_dir(&case).unwrap();
        let copied = case.join("sessions").join("copied");
        fs::create_dir_all(&copied).unwrap();
        let saved = copied.join(original.file_name().unwrap());
        fs::copy(original, &saved).unwrap();
        let progress = PathBuf::from(format!("{}.progress.jsonl", original.display()));
        if progress.exists() {
            fs::copy(
                &progress,
                PathBuf::from(format!("{}.progress.jsonl", saved.display())),
            )
            .unwrap();
        }
        let project = case.join("workspace");
        fs::create_dir(&project).unwrap();
        let service = Service::new(BackendConfig {
            project: project.clone(),
            registry: case.join("registry.json"),
            rpi_bin: executable.clone(),
            ..Default::default()
        })
        .unwrap();
        // The launcher supplies fixed discovery exclusions even for a native
        // without the separate safe-resource-query profile. No plugin is loaded.
        let host = HostSession::spawn_options(SessionOptions {
            executable: launcher.clone(),
            project,
            session_id: id.clone(),
            session_dir: Some(case.join("sessions")),
            saved_session: Some(saved.clone()),
            fork_source: None,
            timings_dir: case.join("timings"),
            model: None,
            provider: None,
            permission_gate: false,
            bypass: false,
            resources: None,
        })
        .await
        .unwrap();
        service
            .sessions
            .write()
            .await
            .insert(id.clone(), host.clone());
        let mut raw = host.subscribe_raw();
        let loaded_hash = digest(&saved);
        let snapshot = child_snapshot(
            &service,
            id.clone(),
            ChildSnapshotQuery {
                children: ChildPageQuery {
                    leaf_id: None,
                    cursor: None,
                    direction: Direction::NewestFirst,
                    limit: 8,
                    max_bytes: 32768,
                },
                phase_limit: 2,
            },
        )
        .await;
        let evidence = match &snapshot {
            Ok(value) => serde_json::to_value(value).unwrap(),
            Err(error) => json!({"kind":format!("{:?}",error.0),"error":error.1}),
        };
        fs::write(
            case.join("snapshot.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        let snapshot = snapshot.unwrap();
        assert!(!snapshot.resync_required);
        assert!(snapshot.events.is_empty());
        for observation in &snapshot.observations {
            let native = expected
                .get(&observation.child_id)
                .expect("exact native phase source");
            let expected = observe(&observation.child_id, native);
            assert_eq!(observation.outcome, expected.outcome);
            assert_eq!(observation.final_phase, expected.final_phase);
            assert_eq!(observation.closed_phase, expected.closed_phase);
        }
        while let Ok(event) = raw.try_recv() {
            if matches!(
                event["type"].as_str(),
                Some(
                    "agent_start" | "operation_accepted" | "tool_execution_start" | "message_start"
                )
            ) {
                execution_events += 1;
            }
        }
        assert_eq!(
            digest(&saved),
            loaded_hash,
            "snapshot queries must not mutate native primary history"
        );
        assert_eq!(
            digest(original),
            original_hash,
            "original evidence must remain unchanged"
        );
        selected_children += snapshot.observations.len();
        results.push(json!({"sessionId":id,"selectedChildren":snapshot.observations.len(),"source":original,
            "sourceSha256":original_hash,"snapshotBytes":serde_json::to_vec(&snapshot).unwrap().len(),"lastSeq":snapshot.last_seq}));
        service.shutdown().await;
        drop(raw);
        drop(host);
        drop(service);
    }
    assert_eq!(execution_events, 0);
    assert_eq!(digest(&executable), expected_hash);
    let result = json!({"status":"PASSED","sessions":results.len(),"selectedChildren":selected_children,
        "executionEvents":execution_events,"hostSha256":expected_hash,"launcherSha256":digest(&launcher),"scope":"read-only bounded core snapshots, not live native-core overlap or adapter/UI acceptance",
        "snapshotSourceSha256":format!("{:x}",Sha256::digest(include_bytes!("../tool_program_snapshot.rs"))),"results":results});
    fs::write(
        output.join("results.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!("{result}");
}
