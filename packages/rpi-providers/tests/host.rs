//! Run explicitly against a built provider-package host; no remote API calls.
use std::{
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn library() -> PathBuf {
    let name = format!(
        "{}rpi_providers{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let executable = std::env::current_exe().unwrap();
    executable
        .ancestors()
        .flat_map(|directory| [directory.join(&name), directory.join("deps").join(&name)])
        .find(|path| path.is_file())
        .expect("first cargo build -p rpi-providers")
}

fn command(temp: &tempfile::TempDir, auth: bool) -> Command {
    let host = std::env::var_os("RPI_TEST_CLI")
        .expect("set RPI_TEST_CLI to the built provider-package host");
    let mut cmd = Command::new(host);
    cmd.current_dir(temp.path())
        .stdin(Stdio::null())
        .env("RPI_CODING_AGENT_DIR", temp.path().join("agent"))
        .env("RPI_OFFLINE", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("DEEPSEEK_API_KEY");
    if auth {
        cmd.args(["auth", "check", "--provider", "deepseek", "--json"]);
    }
    cmd.arg("--extension").arg(library());
    cmd
}

fn check(output: Output, ready: bool, source: &str) {
    assert_eq!(
        output.status.code(),
        Some(if ready { 0 } else { 1 }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ready"], ready);
    assert_eq!(value["source"], source);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a built provider-package CLI in RPI_TEST_CLI; uses only local HTTP"]
async fn host_discovers_and_executes_deepseek_with_provider_scoped_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let listed = command(&temp, false)
        .args(["--list-models", "deepseek"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let catalog = String::from_utf8_lossy(&listed.stdout);
    assert!(catalog.contains("deepseek-v4-pro") && catalog.contains("deepseek-flash"));
    let disabled = command(&temp, false)
        .args(["--no-extensions", "--list-models", "deepseek"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&disabled.stdout).contains("deepseek-flash"));
    check(
        command(&temp, true)
            .env("OPENAI_API_KEY", "fixture-secret-other-provider")
            .output()
            .unwrap(),
        false,
        "none",
    );
    check(
        command(&temp, true)
            .env("DEEPSEEK_API_KEY", "fixture-secret-env")
            .output()
            .unwrap(),
        true,
        "DEEPSEEK_API_KEY",
    );
    check(
        command(&temp, true)
            .env("DEEPSEEK_API_KEY", "fixture-secret-env")
            .arg("--no-extensions")
            .output()
            .unwrap(),
        false,
        "none",
    );

    for source in [
        "DEEPSEEK_API_KEY",
        "models.json",
        "auth.json",
        "headers",
        "oauth",
        "cli",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut provider = serde_json::json!({"baseUrl":base,"headers":{"X-Package-Fixture":"enabled"},"models":[]});
        let key = format!("fixture-secret-{source}");
        if source == "models.json" || source == "cli" {
            provider["apiKey"] = serde_json::json!(if source == "cli" {
                "fixture-secret-weaker-config"
            } else {
                &key
            });
        }
        if source == "headers" {
            provider["headers"]["Authorization"] = serde_json::json!(format!("Bearer {key}"));
        }
        let agent = temp.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("models.json"),
            serde_json::json!({"providers":{"deepseek":provider}}).to_string(),
        )
        .unwrap();
        if source == "auth.json" {
            std::fs::write(
                agent.join("auth.json"),
                serde_json::json!({"deepseek":{"type":"api_key","key":key}}).to_string(),
            )
            .unwrap();
        } else if source == "oauth" {
            std::fs::write(
                agent.join("auth.json"),
                serde_json::json!({"deepseek":{"type":"oauth","access":key,"expires":0}})
                    .to_string(),
            )
            .unwrap();
        }
        if source != "cli" {
            let mut auth = command(&temp, true);
            if source != "headers" {
                auth.env(
                    "DEEPSEEK_API_KEY",
                    if source == "DEEPSEEK_API_KEY" {
                        &key
                    } else {
                        "fixture-secret-weaker-env"
                    },
                );
            }
            check(auth.output().unwrap(), true, source);
        }
        let expected_key = key.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(15), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = vec![];
            let end = loop {
                let mut chunk = [0; 4096];
                let count = socket.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break end;
                    }
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            assert!(headers.starts_with("post /v1/chat/completions "));
            assert!(headers.contains(&format!(
                "authorization: bearer {}",
                expected_key.to_ascii_lowercase()
            )));
            assert!(headers.contains("x-package-fixture: enabled"));
            assert!(!headers.contains("other-provider") && !headers.contains("weaker-env"));
            let body: serde_json::Value = serde_json::from_slice(&bytes[end + 4..]).unwrap();
            assert_eq!(body["model"], "deepseek-v4-pro");
            assert_eq!(
                body["thinking"]["type"], "disabled",
                "must execute the package's policy"
            );
            let reply = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"external deepseek works\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
        });
        let mut cli = command(&temp, false);
        cli.env("OPENAI_API_KEY", "fixture-secret-other-provider")
            .args([
                "--provider",
                "deepseek",
                "--thinking",
                "off",
                "--print",
                "--no-session",
                "--no-tools",
                "hello",
            ]);
        if source != "headers" {
            cli.env(
                "DEEPSEEK_API_KEY",
                if source == "DEEPSEEK_API_KEY" {
                    &key
                } else {
                    "fixture-secret-weaker-env"
                },
            );
        }
        if source == "cli" {
            cli.args(["--api-key", &key]);
        }
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::task::spawn_blocking(move || cli.output().unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("external deepseek works"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
        server.await.unwrap();
    }
}
