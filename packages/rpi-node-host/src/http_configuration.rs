//! Owner-only saved listener settings. Password material is stored as the same
//! verifier used by the HTTP adapter and is never returned in a UI snapshot.
use rpi_http_component::{Configuration, Configure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Saved {
    pub configuration: Configuration,
    pub password_digest: Option<[u8; 32]>,
}
pub(super) fn normalize(mut configuration: Configuration) -> Result<Configuration, String> {
    let bind = configuration
        .bind
        .parse::<std::net::IpAddr>()
        .map_err(|_| "监听地址必须是 IP 地址")?;
    configuration.bind = bind.to_string();
    if !configuration.public_url.is_empty() {
        configuration.public_url = rpi_http_node::public_address(&configuration.public_url)?.0;
    }
    Ok(configuration)
}
fn read(path: &Path) -> Result<Option<Saved>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && meta.len() <= 8192 => {}
        _ => return Err("无法读取 HTTP 设置，请在本机重新保存配置".into()),
    }
    let saved: Saved =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "无法读取 HTTP 设置")?)
            .map_err(|_| "HTTP 设置无效，请在本机重新保存配置")?;
    Ok(Some(saved))
}
pub(super) fn load(path: &Path) -> Result<Option<Saved>, String> {
    let Some(mut saved) = read(path)? else {
        return Ok(None);
    };
    saved.configuration = normalize(saved.configuration)?;
    Ok(Some(saved))
}
pub(super) fn save(path: &Path, request: Configure) -> Result<Configuration, String> {
    let configuration = normalize(request.configuration)?;
    let password_digest = if let Some(password) = request.new_password {
        if password.is_empty() || password.len() > 512 {
            return Err("节点密码须为 1 到 512 字节".into());
        }
        Some(Sha256::digest(password.as_bytes()).into())
    } else {
        read(path).ok().flatten().and_then(|s| s.password_digest)
    };
    let saved = Saved {
        configuration: configuration.clone(),
        password_digest,
    };
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or("无法定位 HTTP 设置目录")?)
        .map_err(|_| "无法保存 HTTP 设置")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "无法设置 HTTP 配置权限")?;
    }
    temp.write_all(&serde_json::to_vec(&saved).map_err(|_| "无法编码 HTTP 设置")?)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|_| "无法保存 HTTP 设置")?;
    temp.persist(path).map_err(|_| "无法更新 HTTP 设置")?;
    Ok(configuration)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_requests_preserve_settings_and_credentials_never_appear_in_form_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("http.json");
        let config = Configuration {
            bind: "127.0.0.1".into(),
            port: 14000,
            public_url: String::new(),
        };
        save(
            &path,
            Configure {
                configuration: config.clone(),
                new_password: Some("fixture-private-only".into()),
            },
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("fixture-private-only"));
        assert!(load(&path).unwrap().unwrap().password_digest.is_some());
        let mut invalid = config.clone();
        invalid.bind = "0.0.0.0;command".into();
        assert!(
            save(
                &path,
                Configure {
                    configuration: invalid,
                    new_password: None
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        save(
            &path,
            Configure {
                configuration: config,
                new_password: None,
            },
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
