use super::*;
use std::collections::BTreeMap;

fn recompute(bundle: &mut PackageBundle) {
    bundle.sha256 = digest(&serde_json::to_vec(&(&bundle.manifest, &bundle.sources)).unwrap());
}
fn owner(root: &tempfile::TempDir) -> LocalWorkspace {
    LocalWorkspace::with_session_dir(
        root.path().join("never-launch-host"),
        root.path().join("data"),
        Some(root.path().join("sessions")),
    )
    .unwrap()
}

#[test]
fn curated_sources_roundtrip_and_metadata_digest_are_fixed() {
    for bundle in curated().unwrap() {
        validate(&bundle).unwrap();
        let json = serde_json::to_vec(&bundle).unwrap();
        let decoded: PackageBundle = serde_json::from_slice(&json).unwrap();
        assert_eq!(bundle, decoded);
        assert_eq!(resolve(&reference(&bundle)).unwrap(), bundle);
        let mut changed = bundle.clone();
        changed.manifest.author = "another author".into();
        assert!(validate(&changed).is_err());
        let mut changed = bundle.clone();
        changed.sources.values_mut().next().unwrap().push('!');
        assert!(validate(&changed).is_err());
    }
    let packages = curated().unwrap();
    assert_eq!(
        packages[0].sources,
        BASE.iter()
            .map(|(p, t)| ((*p).into(), (*t).into()))
            .collect()
    );
    assert_eq!(
        packages[1].sources,
        DEVELOPER
            .iter()
            .map(|(p, t)| ((*p).into(), (*t).into()))
            .collect()
    );
}

#[test]
fn rejects_traversal_aliases_binaries_and_capacity_before_writes() {
    for path in [
        "../x.md",
        "/x.md",
        "a/../x.md",
        "a//x.md",
        "a/./x.md",
        "a\\x.md",
        "C:/x.md",
        "x.md:ads",
        "a/.x.md",
        "a /x.md",
        "a./x.md",
        "CON.md",
        "lpt1.md",
        "NUL/x.md",
        "x.dll",
        "build.rs",
        "setup.sh",
    ] {
        assert!(!source_path(path), "{path}");
    }
    for path in [
        "SKILL.md",
        "prompts/system.md",
        "skills/work-basics/SKILL.md",
    ] {
        assert!(source_path(path), "{path}");
    }
    let good = curated().unwrap().remove(0);
    let mut changed = good.clone();
    changed
        .sources
        .insert("PROMPTS/SYSTEM.md".into(), "alias".into());
    recompute(&mut changed);
    assert!(validate(&changed).is_err());
    let mut changed = good.clone();
    changed.sources.insert("a.md".into(), "file".into());
    changed
        .sources
        .insert("a.md/nested.md".into(), "file".into());
    recompute(&mut changed);
    assert!(validate(&changed).is_err());
    let mut changed = good.clone();
    changed.manifest.kind = NativeKind::Extension;
    recompute(&mut changed);
    assert!(validate(&changed).is_err());
    let mut changed = good.clone();
    changed
        .sources
        .insert("large.md".into(), "x".repeat(256 * 1024 + 1));
    recompute(&mut changed);
    assert!(validate(&changed).is_err());
    let mut changed = good;
    changed
        .sources
        .extend((0..65).map(|n| (format!("{n}.md"), "x".into())));
    recompute(&mut changed);
    assert!(validate(&changed).is_err());
}

#[test]
fn skill_metadata_and_instruction_entries_must_be_canonical() {
    let mut bundles = curated().unwrap();
    let mut skill = bundles.pop().unwrap();
    skill.sources.insert(
        "SKILL.md".into(),
        "---\nname: another\ndescription: test\n---\nbody\n".into(),
    );
    recompute(&mut skill);
    assert!(validate(&skill).is_err());
    let mut package = bundles.remove(0);
    package.manifest.instructions = vec!["missing.md".into()];
    recompute(&mut package);
    assert!(validate(&package).is_err());
}

#[tokio::test]
async fn acquisition_is_pinned_idempotent_and_not_an_install_or_host() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(&root);
    let project = owner
        .create_project("package-test", None, false)
        .await
        .unwrap();
    let first = owner.package_catalog(&project.id).await.unwrap();
    assert!(first.packages.iter().all(|p| !p.acquired));
    for bundle in curated().unwrap() {
        let target = reference(&bundle);
        assert_eq!(
            owner
                .package_bundle(&project.id, target.clone())
                .await
                .unwrap(),
            bundle
        );
        assert!(!owner.package_source(&bundle).exists());
        let mut bad = target.clone();
        bad.sha256 = "0".repeat(64);
        assert!(owner.package_acquire(&project.id, bad).await.is_err());
        assert!(!owner.package_source(&bundle).exists());
        let receipt = owner
            .package_acquire(&project.id, target.clone())
            .await
            .unwrap();
        assert_eq!(receipt.package, target);
        assert!(verify_tree(Path::new(&receipt.source), &bundle).unwrap());
        assert_eq!(
            owner
                .package_acquire(&project.id, target)
                .await
                .unwrap()
                .source,
            receipt.source
        );
    }
    assert!(
        owner
            .package_catalog(&project.id)
            .await
            .unwrap()
            .packages
            .iter()
            .all(|p| p.acquired)
    );
    // No discovery/acquisition operation has created even a native Service.
    assert!(owner.lock().unwrap().services.is_empty());
    assert_eq!(owner.sessions().await.unwrap(), []);
    owner.shutdown().await;
    assert!(owner.package_catalog(&project.id).await.is_err());
}

#[tokio::test]
async fn incomplete_bytes_can_complete_but_changed_and_extra_bytes_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(&root);
    let project = owner.create_project("partial", None, false).await.unwrap();
    let bundle = curated().unwrap().remove(0);
    let source = owner.package_source(&bundle);
    let (file, text) = bundle.sources.first_key_value().unwrap();
    immutable(&source.join(file), text.as_bytes()).unwrap();
    assert!(!verify_tree(&source, &bundle).unwrap());
    owner
        .package_acquire(&project.id, reference(&bundle))
        .await
        .unwrap();
    assert!(verify_tree(&source, &bundle).unwrap());
    std::fs::write(source.join(file), b"external bytes").unwrap();
    assert!(
        owner
            .package_acquire(&project.id, reference(&bundle))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(source.join(file)).unwrap(), b"external bytes");
    let catalog = owner.package_catalog(&project.id).await.unwrap();
    assert!(!catalog.packages[0].acquired);
    assert_ne!(catalog.diagnostics, [] as [String; 0]);
    std::fs::write(source.join(file), text).unwrap();
    std::fs::write(source.join("untrusted.dll"), b"not loaded").unwrap();
    assert!(
        owner
            .package_acquire(&project.id, reference(&bundle))
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(source.join("untrusted.dll")).unwrap(),
        b"not loaded"
    );
    assert!(owner.lock().unwrap().services.is_empty());
    owner.shutdown().await;
}

#[tokio::test]
async fn authorization_does_not_fall_back_or_acquire_on_install() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(&root);
    let project = owner
        .create_project("authorized", None, false)
        .await
        .unwrap();
    let bundle = curated().unwrap().remove(0);
    assert!(owner.package_catalog("unknown-project").await.is_err());
    assert!(
        owner
            .package_acquire("unknown-project", reference(&bundle))
            .await
            .is_err()
    );
    for trusted in [false, true] {
        assert!(
            owner
                .package_install(
                    &project.id,
                    PackageInstall {
                        package: reference(&bundle),
                        scope: rpi_node_api::api::NativeScope::Project,
                        trusted
                    }
                )
                .await
                .is_err()
        );
    }
    assert!(!owner.package_source(&bundle).exists());
    assert!(owner.lock().unwrap().services.is_empty());
    owner.shutdown().await;
}

#[tokio::test]
async fn concurrent_acquisition_is_one_content_identity() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(&root);
    let project = owner
        .create_project("concurrent", None, false)
        .await
        .unwrap();
    let bundle = curated().unwrap().remove(0);
    let (a, b) = tokio::join!(
        owner.package_acquire(&project.id, reference(&bundle)),
        owner.package_acquire(&project.id, reference(&bundle))
    );
    assert_eq!(a.unwrap().source, b.unwrap().source);
    assert!(verify_tree(&owner.package_source(&bundle), &bundle).unwrap());
    assert!(owner.lock().unwrap().services.is_empty());
    owner.shutdown().await;
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn symlink_or_junction_sources_are_not_followed() {
    let root = tempfile::tempdir().unwrap();
    let owner = owner(&root);
    let project = owner.create_project("links", None, false).await.unwrap();
    let bundle = curated().unwrap().remove(0);
    let source = owner.package_source(&bundle);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &source).unwrap();
    #[cfg(windows)]
    {
        // Junctions do not require enabling Developer Mode/admin symlink policy.
        let result = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&source)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(result.status.success());
    }
    assert!(
        owner
            .package_acquire(&project.id, reference(&bundle))
            .await
            .is_err()
    );
    assert!(!owner.package_catalog(&project.id).await.unwrap().packages[0].acquired);
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    assert!(owner.lock().unwrap().services.is_empty());
    // Remove only the test-owned directory link, not its target.
    #[cfg(windows)]
    std::fs::remove_dir(&source).unwrap();
    #[cfg(unix)]
    std::fs::remove_file(&source).unwrap();
    owner.shutdown().await;
}

#[test]
fn distribution_wire_rejects_unknown_fields() {
    let reference = reference(&curated().unwrap().remove(0));
    let mut json = serde_json::to_value(&reference).unwrap();
    json["url"] = serde_json::json!("https://invalid.example/");
    assert!(serde_json::from_value::<PackageRef>(json).is_err());
    let mut bundle = serde_json::to_value(curated().unwrap().remove(0)).unwrap();
    bundle["manifest"]["executable"] = true.into();
    assert!(serde_json::from_value::<PackageBundle>(bundle).is_err());
    // Keep an explicit sorted-map type in the digest contract.
    let _: BTreeMap<String, String> = curated().unwrap().remove(0).sources;
}

#[tokio::test]
async fn cancelled_io_is_retained_and_shutdown_waits_for_actual_worker_exit() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            let owner = owner(&root);
            let project = owner.create_project("retained", None, false).await.unwrap();
            let source = owner.package_source(&curated().unwrap().remove(0));
            let (started, wait_started) = tokio::sync::oneshot::channel();
            let (release, wait_release) = std::sync::mpsc::channel();
            let (finished, wait_finished) = tokio::sync::oneshot::channel();
            let child_owner = owner.clone();
            let caller = tokio::task::spawn_local(async move {
                child_owner
                    .package_io(&project.id, move |owner| {
                        let _ = started.send(());
                        wait_release
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        let bundle = curated()?.remove(0);
                        let target = owner.package_source(&bundle);
                        for (path, text) in &bundle.sources {
                            immutable(&target.join(path), text.as_bytes())?;
                        }
                        assert!(verify_tree(&target, &bundle)?);
                        let _ = finished.send(());
                        Ok(())
                    })
                    .await
            });
            wait_started.await.unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert!(owner.inner.lifecycle.try_write().is_err());
            let shutting_down = owner.clone();
            let stop = tokio::task::spawn_local(async move {
                shutting_down.shutdown().await;
            });
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            assert!(
                !stop.is_finished(),
                "shutdown must not release owner before blocking IO exits"
            );
            release.send(()).unwrap();
            wait_finished.await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), stop)
                .await
                .unwrap()
                .unwrap();
            assert!(verify_tree(&source, &curated().unwrap().remove(0)).unwrap());
            assert!(owner.inner.lifecycle.try_write().is_ok());
        })
        .await;
}

#[cfg(unix)]
#[test]
fn literal_backslash_aliases_never_impersonate_a_canonical_tree() {
    let root = tempfile::tempdir().unwrap();
    let bundle = curated().unwrap().remove(0);
    for (path, text) in &bundle.sources {
        std::fs::write(root.path().join(path.replace('/', "\\")), text).unwrap();
    }
    assert!(verify_tree(root.path(), &bundle).is_err());
    // A complete canonical tree plus flat aliases must still reject extra files.
    for (path, text) in &bundle.sources {
        immutable(&root.path().join(path), text.as_bytes()).unwrap();
    }
    assert!(verify_tree(root.path(), &bundle).is_err());
    assert!(root.path().join(r"prompts\system.md").is_file());
}
