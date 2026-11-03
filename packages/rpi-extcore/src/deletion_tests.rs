//! Synthetic fixtures only; never use configured native storage or user data.
use super::*;
use std::io::Write;

struct Fixture {
    dir: PathBuf,
    root: PathBuf,
    timings: PathBuf,
    id: String,
    main: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        // Canonicalize first: e.g. macOS /var can itself be a system symlink.
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let dir = base.join(format!("rpi-deletion-test-{}", uuid::Uuid::new_v4()));
        let root = dir.join("sessions");
        let timings = dir.join("timings");
        let id = uuid::Uuid::new_v4().to_string();
        let main = root
            .join("--fixture-cwd--")
            .join(format!("2026-05-10T00-00-00_{id}.jsonl"));
        fs::create_dir_all(main.parent().unwrap()).unwrap();
        fs::create_dir_all(&timings).unwrap();
        Self {
            dir,
            root,
            timings,
            id,
            main,
        }
    }
    fn write_main(&self) {
        self.write_header(json!({"kind":"header","version":4,"id":self.id,"progressFormat":"assistant-progress-v1"}));
    }
    fn write_header(&self, header: Value) {
        fs::write(
            &self.main,
            format!("{header}\nTHIS BODY IS NOT JSON AND MUST NOT BE READ\n"),
        )
        .unwrap();
    }
    fn companions(&self) -> Vec<PathBuf> {
        vec![
            append(&self.main, ".tmp"),
            append(&self.main, ".progress.jsonl"),
            append(&self.main, ".progress.jsonl.tmp"),
            self.timings.join(format!("{}.json", self.id)),
            self.timings.join(format!("{}.json.tmp", self.id)),
        ]
    }
    fn populate(&self) {
        self.write_main();
        for p in self.companions() {
            fs::write(p, b"synthetic sidecar").unwrap();
        }
    }
    fn plan(&self) -> DeletionPlan {
        prepare(&self.root, &self.id, &self.timings).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn code<T: std::fmt::Debug>(result: ServiceResult<T>, expected: &str) {
    let (kind, value) = result.unwrap_err();
    assert_eq!(value["code"], expected, "{kind:?}: {value}");
    let text = value["error"].as_str().unwrap();
    assert!(!text.contains("fixture"));
    assert!(!text.contains("synthetic"));
}

#[test]
fn deletes_exact_history_progress_and_known_timing_tmp_keeps_neighbors_and_lock() {
    let f = Fixture::new();
    f.populate();
    let cwd = f.main.parent().unwrap();
    let parent = cwd.join(format!("parent_{}.jsonl", uuid::Uuid::new_v4()));
    let fork = cwd.join(format!("fork_{}.jsonl", uuid::Uuid::new_v4()));
    let attachment = cwd.join("external-attachment.png");
    let unknown = append(&f.main, ".unrecognized");
    for p in [&parent, &fork, &attachment, &unknown] {
        fs::write(p, b"keep").unwrap();
    }
    let lock = append(&f.main, ".lock");
    fs::write(&lock, b"stable lock contents").unwrap();
    let plan = f.plan();
    assert_eq!(plan.files.len(), 6);
    assert_eq!(
        plan.primary,
        Some(f.main.strip_prefix(&f.root).unwrap().to_owned())
    );
    plan.validate(&f.id).unwrap();
    let serialized = serde_json::to_string(&plan).unwrap();
    assert!(!serialized.contains("THIS BODY"));
    assert!(!serialized.contains("synthetic sidecar"));
    let persisted: DeletionPlan = serde_json::from_str(&serialized).unwrap();
    execute(&persisted).unwrap();
    execute(&persisted).unwrap();
    assert!(!f.main.exists());
    assert!(f.companions().iter().all(|p| !p.exists()));
    for p in [&parent, &fork, &attachment, &unknown, &lock] {
        assert!(p.is_file());
    }
    assert_eq!(fs::read(lock).unwrap(), b"stable lock contents");
    assert!(cwd.is_dir());
}

#[test]
fn missing_root_and_no_match_fail_closed_without_creating_directories() {
    let f = Fixture::new();
    let absent = f.dir.join("never-created").join("sessions");
    let absent_timing = f.dir.join("never-created-timings");
    code(
        prepare(&absent, &f.id, &absent_timing),
        "deletion_history_missing",
    );
    code(
        prepare_locked(&absent, &f.id, &absent_timing),
        "deletion_history_missing",
    );
    assert!(!absent.exists());
    assert!(!absent_timing.exists());
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_history_missing",
    );
    assert!(f.root.is_dir());
    assert!(f.main.parent().unwrap().is_dir());
}

#[test]
fn registry_only_entry_cannot_retire_timing_without_native_history() {
    let f = Fixture::new();
    let timing = f.timings.join(format!("{}.json", f.id));
    fs::write(&timing, b"{}").unwrap();
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_history_missing",
    );
    code(
        prepare_locked(&f.root, &f.id, &f.timings),
        "deletion_history_missing",
    );
    let missing = f.dir.join("missing-root");
    code(
        prepare_locked(&missing, &f.id, &f.timings),
        "deletion_history_missing",
    );
    assert_eq!(fs::read(timing).unwrap(), b"{}");
}

#[test]
fn only_canonical_uuid_is_admitted() {
    let f = Fixture::new();
    for id in [
        "",
        "../session",
        "not-a-uuid",
        "550E8400-E29B-41D4-A716-446655440000",
        "550e8400e29b41d4a716446655440000",
        "{550e8400-e29b-41d4-a716-446655440000}",
    ] {
        code(prepare(&f.root, id, &f.timings), "deletion_invalid_id");
    }
    f.write_main();
    let plan = f.plan();
    assert!(plan.validate(&uuid::Uuid::new_v4().to_string()).is_err());
}

#[test]
fn duplicate_exact_suffix_is_rejected_before_any_deletion() {
    let f = Fixture::new();
    f.populate();
    let duplicate = f.root.join(format!("other_{}.jsonl", f.id));
    fs::copy(&f.main, &duplicate).unwrap();
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_duplicate_history",
    );
    assert!(f.main.exists());
    assert!(duplicate.exists());
}

#[test]
fn exact_suffix_does_not_match_bare_uuid_or_similar_names() {
    let f = Fixture::new();
    for name in [
        format!("{}.jsonl", f.id),
        format!("prefix_{}.jsonl.bak", f.id),
    ] {
        fs::write(f.main.parent().unwrap().join(name), b"unrelated").unwrap();
    }
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_history_missing",
    );
}

#[test]
fn header_mismatch_unsupported_progress_and_oversized_header_fail_closed() {
    let f = Fixture::new();
    for h in [
        json!({"kind":"header","version":4,"id":uuid::Uuid::new_v4().to_string()}),
        json!({"kind":"header","version":3,"id":f.id}),
        json!({"kind":"message","version":4,"id":f.id}),
        json!({"kind":"header","version":4,"id":f.id,"progressFormat":"unknown-v2"}),
        json!({"kind":"header","version":4,"id":f.id,"progressFormat":null}),
    ] {
        f.write_header(h);
        code(
            prepare(&f.root, &f.id, &f.timings),
            "deletion_unknown_history",
        );
    }
    fs::write(&f.main, vec![b'x'; HEADER_LIMIT + 1]).unwrap();
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_unknown_history",
    );
    f.write_header(json!({"kind":"header","version":4,"id":f.id}));
    assert!(f.plan().primary.is_some());
}

#[test]
fn orphan_progress_is_unknown_not_an_empty_cleanup_claim() {
    let f = Fixture::new();
    let progress = append(&f.main, ".progress.jsonl");
    fs::write(&progress, b"unknown orphan").unwrap();
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_unknown_history",
    );
    assert!(progress.exists());
}

#[test]
fn uuid_cwd_layout_keeps_other_uuid_directories() {
    let mut f = Fixture::new();
    f.main = f
        .root
        .join(&f.id)
        .join("--fixture-cwd--")
        .join(format!("time_{}.jsonl", f.id));
    fs::create_dir_all(f.main.parent().unwrap()).unwrap();
    f.populate();
    let other = uuid::Uuid::new_v4().to_string();
    let neighbor = f
        .root
        .join(&other)
        .join("--neighbor--")
        .join(format!("time_{other}.jsonl"));
    fs::create_dir_all(neighbor.parent().unwrap()).unwrap();
    fs::write(&neighbor, b"keep neighbor body").unwrap();
    execute(&f.plan()).unwrap();
    assert!(neighbor.exists());
}

#[test]
fn directory_scan_is_finite_and_rejects_over_budget() {
    let f = Fixture::new();
    for i in 0..MAX_DIR_ENTRIES {
        File::create(f.root.join(format!("unrelated-{i}"))).unwrap();
    }
    // The preexisting encoded-cwd directory is the 10001st entry.
    code(prepare(&f.root, &f.id, &f.timings), "deletion_scan_limit");
}

#[test]
fn native_exclusive_lock_conflicts_without_deleting_companions() {
    let f = Fixture::new();
    f.populate();
    let plan = f.plan();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(append(&f.main, ".lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&lock).unwrap();
    let error = execute(&plan).unwrap_err();
    assert_eq!(error.0, ReplyKind::Conflict);
    assert_eq!(error.1["code"], "session_in_use");
    assert!(f.main.exists());
    assert!(f.companions().iter().all(|p| p.exists()));
    FileExt::unlock(&lock).unwrap();
    execute(&plan).unwrap();
}

#[test]
fn changed_companion_prevents_primary_deletion() {
    let f = Fixture::new();
    f.populate();
    let plan = f.plan();
    fs::write(
        append(&f.main, ".progress.jsonl"),
        b"changed fingerprint and length",
    )
    .unwrap();
    code(execute(&plan), "deletion_identity_changed");
    assert!(f.main.exists());
    assert!(f.companions().iter().all(|p| p.exists()));
}

#[test]
fn changed_main_and_late_sidecar_are_not_deleted() {
    let f = Fixture::new();
    f.write_main();
    let plan = f.plan();
    let late = append(&f.main, ".progress.jsonl");
    fs::write(&late, b"late").unwrap();
    code(execute(&plan), "deletion_identity_changed");
    assert!(f.main.exists());
    assert!(late.exists());
    fs::remove_file(late).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&f.main)
        .unwrap()
        .write_all(b"new data")
        .unwrap();
    code(execute(&plan), "deletion_identity_changed");
    assert!(f.main.exists());
}

#[test]
fn persisted_manifest_cannot_escape_or_pick_an_arbitrary_file() {
    let f = Fixture::new();
    f.populate();
    let plan = f.plan();
    for primary in [
        PathBuf::from("../escape.jsonl"),
        f.main.clone(),
        PathBuf::from("cwd/../../escape.jsonl"),
        PathBuf::from("cwd/arbitrary.json"),
        PathBuf::from(format!("wrong-owner/cwd/time_{}.jsonl", f.id)),
    ] {
        let mut bad = plan.clone();
        bad.primary = Some(primary);
        assert!(bad.validate(&f.id).is_err());
        code(execute(&bad), "deletion_invalid_plan");
    }
    let mut bad = plan.clone();
    bad.root.path = f.root.join("..").join("outside");
    // Windows verbatim PathBuf::join normalizes `..` before validate sees it.
    // Regardless of lexical normalization, the persisted directory identity
    // must reject the redirected root at execution, before any unlink.
    assert!(execute(&bad).is_err());
    assert!(f.main.exists());
    assert!(f.companions().iter().all(|path| path.exists()));
    let mut bad = plan.clone();
    bad.files.push(bad.files[0].clone());
    assert!(bad.validate(&f.id).is_err());
    let mut value = serde_json::to_value(plan).unwrap();
    value["files"][0]["path"] = json!("arbitrary");
    assert!(serde_json::from_value::<DeletionPlan>(value).is_err());
    assert!(f.main.exists());
}

#[test]
fn main_failure_does_not_delete_companions_and_same_manifest_retries() {
    let f = Fixture::new();
    f.populate();
    let plan = f.plan();
    FAIL_REMOVE.with(|v| v.set(Some(FileKind::Primary)));
    code(execute(&plan), "deletion_io");
    assert!(f.main.exists());
    assert!(f.companions().iter().all(|p| p.exists()));
    execute(&plan).unwrap();
}

#[test]
fn partial_companion_failure_and_already_missing_primary_resume_same_plan() {
    let f = Fixture::new();
    f.populate();
    let plan = f.plan();
    FAIL_REMOVE.with(|v| v.set(Some(FileKind::Progress)));
    code(execute(&plan), "deletion_io");
    assert!(!f.main.exists());
    assert!(!append(&f.main, ".tmp").exists());
    assert!(append(&f.main, ".progress.jsonl").exists());
    assert!(f.timings.join(format!("{}.json", f.id)).exists());
    // Preparing a replacement intent would reject the orphan; only persisted
    // original fingerprints authorize this recovery.
    code(
        prepare(&f.root, &f.id, &f.timings),
        "deletion_unknown_history",
    );
    let restored = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
    execute(&restored).unwrap();
    execute(&restored).unwrap();
    assert!(f.companions().iter().all(|p| !p.exists()));
}

#[test]
fn persisted_legacy_empty_plan_still_rejects_new_history_or_root() {
    let f = Fixture::new();
    // Old serialized manifests remain readable; never prepare new empty plans.
    let legacy = |root: &Path| DeletionPlan {
        version: 1,
        id: f.id.clone(),
        root: anchor(root).unwrap(),
        timings: anchor(&f.timings).unwrap(),
        primary: None,
        files: Vec::new(),
    };
    let plan = legacy(&f.root);
    f.write_main();
    code(execute(&plan), "deletion_identity_changed");
    assert!(f.main.exists());
    let missing = f.dir.join("new-root");
    let plan = legacy(&missing);
    fs::create_dir(&missing).unwrap();
    code(execute(&plan), "deletion_identity_changed");
}

#[cfg(unix)]
#[test]
fn symlink_root_primary_companion_and_lock_are_rejected_without_following() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.populate();
    let alias = f.dir.join("root-link");
    symlink(&f.root, &alias).unwrap();
    code(prepare(&alias, &f.id, &f.timings), "deletion_unsafe_path");
    let outside = f.dir.join("outside");
    fs::write(&outside, b"outside").unwrap();
    let progress = append(&f.main, ".progress.jsonl");
    fs::remove_file(&progress).unwrap();
    symlink(&outside, &progress).unwrap();
    code(prepare(&f.root, &f.id, &f.timings), "deletion_unsafe_path");
    fs::remove_file(&progress).unwrap();
    fs::write(&progress, b"progress").unwrap();
    let plan = f.plan();
    symlink(&outside, append(&f.main, ".lock")).unwrap();
    code(execute(&plan), "deletion_unsafe_path");
    assert!(f.main.exists());
    fs::remove_file(append(&f.main, ".lock")).unwrap();
    fs::remove_file(&f.main).unwrap();
    symlink(&outside, &f.main).unwrap();
    code(prepare(&f.root, &f.id, &f.timings), "deletion_unsafe_path");
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
}

#[test]
fn prepared_owner_retains_native_lock_across_threads_and_executes_without_relocking() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<PreparedDeletion>();
    let f = Fixture::new();
    f.populate();
    let prepared = prepare_locked(&f.root, &f.id, &f.timings).unwrap();
    let serialized = serde_json::to_value(prepared.plan()).unwrap();
    code(prepare_locked(&f.root, &f.id, &f.timings), "session_in_use");
    // Models moving from the blocking preparer to async owner to blocking executor.
    let prepared = std::thread::spawn(move || prepared).join().unwrap();
    assert_eq!(serde_json::to_value(prepared.plan()).unwrap(), serialized);
    execute_prepared(prepared).unwrap();
    assert!(!f.main.exists());
    assert!(f.companions().iter().all(|p| !p.exists()));
}

#[test]
fn partial_persisted_manifest_never_refreshes_surviving_fingerprints() {
    let f = Fixture::new();
    f.populate();
    let prepared = prepare_locked(&f.root, &f.id, &f.timings).unwrap();
    let persisted = serde_json::to_vec(prepared.plan()).unwrap();
    FAIL_REMOVE.with(|v| v.set(Some(FileKind::Progress)));
    code(execute_prepared(prepared), "deletion_io");
    assert!(!f.main.exists());
    let progress = append(&f.main, ".progress.jsonl");
    fs::write(&progress, b"external replacement after partial removal").unwrap();
    let plan: DeletionPlan = serde_json::from_slice(&persisted).unwrap();
    code(execute(&plan), "deletion_identity_changed");
    assert_eq!(serde_json::to_vec(&plan).unwrap(), persisted);
    assert!(progress.exists());
    assert!(f.timings.join(format!("{}.json", f.id)).exists());
}
