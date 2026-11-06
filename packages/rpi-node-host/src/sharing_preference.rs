//! One non-secret host preference, separate from node selection and credentials.
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    allow_sharing: bool,
}

pub(super) fn load(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        // First-run LAN access is the desktop default; an explicit saved false
        // or any unreadable/invalid preference below still fails closed.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        // A dangling link is an invalid saved preference, not a first run.
        Ok(meta) if meta.is_file() && meta.len() <= 4096 => {}
        _ => return Err("无法读取共享设置，已保持关闭".into()),
    }
    let text = std::fs::read_to_string(path).map_err(|_| "无法读取共享设置，已保持关闭")?;
    serde_json::from_str::<Preference>(&text)
        .map(|p| p.allow_sharing)
        .map_err(|_| "共享设置损坏，已保持关闭".into())
}
pub(super) fn save(path: &Path, allow_sharing: bool) -> Result<(), String> {
    let directory = path.parent().ok_or("无法定位共享设置目录")?;
    let bytes =
        serde_json::to_vec(&Preference { allow_sharing }).map_err(|_| "无法编码共享设置")?;
    let mut temp =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| "无法保存共享设置，开关未更改")?;
    temp.write_all(&bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|_| "无法保存共享设置，开关未更改")?;
    temp.persist(path)
        .map_err(|_| "无法替换共享设置，开关未更改")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_run_defaults_to_sharing_but_invalid_preferences_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sharing.json");
        assert!(load(&path).unwrap());
        for value in [
            "",
            "{}",
            "true",
            "{\"allow_sharing\":\"true\"}",
            "{\"allow_sharing\":true,\"password\":\"no\"}",
        ] {
            std::fs::write(&path, value).unwrap();
            assert!(load(&path).is_err());
        }
    }
    #[test]
    fn non_file_and_oversized_saved_preferences_never_become_first_run() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sharing.json");
        std::fs::create_dir(&path).unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, " ".repeat(4097)).unwrap();
        assert!(load(&path).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn dangling_saved_preference_is_not_a_missing_preference() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sharing.json");
        std::os::unix::fs::symlink(root.path().join("missing"), &path).unwrap();
        assert!(load(&path).is_err());
    }
    #[test]
    fn preference_round_trip_is_atomic_and_contains_no_credentials() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sharing.json");
        save(&path, true).unwrap();
        assert!(load(&path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"allow_sharing\":true}"
        );
        save(&path, false).unwrap();
        assert!(!load(&path).unwrap());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(save(&root.path().join("missing/sharing.json"), true).is_err());
        assert!(!load(&path).unwrap());
    }
}
