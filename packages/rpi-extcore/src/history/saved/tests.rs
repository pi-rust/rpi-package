use super::*;
use serde_json::json;
#[test]
fn owner_location_is_exact_bounded_unambiguous_and_header_only() {
    let root = std::env::temp_dir().join(format!("saved-owner-location-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("project")).unwrap();
    let path = root.join("project/time_session.jsonl");
    let header =
        json!({"kind":"header","version":4,"id":"session","createdAt":0,"cwd":root,"metadata":{}});
    std::fs::write(&path, format!("{header}\nnot decoded by owner location\n")).unwrap();
    assert_eq!(
        locate(&root, "session").unwrap(),
        std::fs::canonicalize(&path).unwrap()
    );
    assert!(locate(&root, "missing").is_err());
    let duplicate = root.join("other_session.jsonl");
    std::fs::copy(&path, &duplicate).unwrap();
    assert_eq!(
        locate(&root, "session").unwrap_err().kind,
        ResourceErrorKind::Conflict
    );
    std::fs::remove_file(duplicate).unwrap();
    let mut wrong = header;
    wrong["id"] = json!("not-session");
    std::fs::write(&path, format!("{wrong}\n")).unwrap();
    assert!(locate(&root, "session").is_err());
    std::fs::remove_file(path).unwrap();
    for n in 0..256 {
        std::fs::create_dir(root.join(format!("d{n}"))).unwrap();
    }
    assert_eq!(
        locate(&root, "session").unwrap_err().kind,
        ResourceErrorKind::LimitExceeded
    );
    std::fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn owner_location_never_traverses_directory_or_file_symlinks() {
    let root = std::env::temp_dir().join(format!("saved-owner-link-{}", uuid::Uuid::new_v4()));
    let outside = root.with_extension("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let path = outside.join("x_session.jsonl");
    std::fs::write(&path,json!({"kind":"header","version":4,"id":"session","createdAt":0,"cwd":outside,"metadata":{}}).to_string()+"\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("project")).unwrap();
    assert!(locate(&root, "session").is_err());
    std::os::unix::fs::symlink(&path, root.join("x_session.jsonl")).unwrap();
    assert_eq!(
        locate(&root, "session").unwrap_err().kind,
        ResourceErrorKind::Conflict
    );
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}
