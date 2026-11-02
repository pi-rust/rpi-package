use super::*;
#[cfg(windows)]
#[tokio::test]
async fn rpc_eof_is_not_exit_and_reader_abort_cannot_drop_the_owned_reaper() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let root = std::env::temp_dir().join(format!("resource-child-exit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let lease = root.join("writer.lease");
    let script = r#"import os, sys, time, msvcrt
f=open(sys.argv[1], 'w+b', buffering=0)
f.write(b'x'); f.seek(0); msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
print('ready', flush=True)
os.close(1)
time.sleep(60)
"#;
    // The Windows venv launcher retains inherited stdout while waiting for its
    // real Python child. Use the actual interpreter so EOF is intentional.
    let probe = std::process::Command::new("python")
        .args(["-c", "import sys;print(sys._base_executable)"])
        .output()
        .unwrap();
    assert!(probe.status.success());
    let executable = String::from_utf8(probe.stdout).unwrap();
    let mut command = std::process::Command::new(executable.trim());
    command
        .args(["-c", script])
        .arg(&lease)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut tree = rpi_mcp_core::ProcessTree::spawn(&mut command).unwrap();
    let output =
        tokio::process::ChildStdout::from_std(tree.child_mut().stdout.take().unwrap()).unwrap();
    let owner = NativeChild::start(tree);
    let mut reader = BufReader::new(output);
    let mut ready = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready.trim(), "ready");
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(tail.is_empty());
    assert!(
        std::fs::read(&lease).is_err(),
        "RPC EOF must not be mistaken for writer-lock release"
    );
    let reader = tokio::spawn(async move {
        drop(reader);
        futures_util::future::pending::<()>().await
    });
    reader.abort();
    let _ = reader.await;
    assert!(
        std::fs::read(&lease).is_err(),
        "aborting RPC owner must not infer OS exit"
    );
    owner.stop_checked().await.unwrap();
    assert_eq!(std::fs::read(&lease).unwrap(), b"x");
    std::fs::remove_dir_all(root).unwrap();
}
