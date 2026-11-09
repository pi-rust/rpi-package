//! Test-only transparent native pipe recorder for the live ToolProgram proof.
//! Explicit binaries/environment only; never synthesizes replies or tool events.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let dir = args
        .windows(2)
        .find(|pair| pair[0] == "--session-dir")
        .map(|pair| std::path::PathBuf::from(&pair[1]))
        .expect("explicit session directory");
    fs::create_dir_all(&dir).expect("isolated session directory");
    let dir = dir.parent().expect("isolated evidence root").to_path_buf();
    let mut child =
        Command::new(std::env::var_os("RPI_CODEMODE_NATIVE_BIN").expect("explicit native"))
            .args([
                "--no-skills",
                "--no-context-files",
                "--no-prompt-templates",
                "--approve",
            ])
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("native launch");
    fs::write(dir.join("native-pid.txt"), child.id().to_string()).unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut sent = fs::File::create(dir.join("native-input.jsonl")).unwrap();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let line = line.unwrap();
            writeln!(sent, "{line}").unwrap();
            sent.flush().unwrap();
            if writeln!(input, "{line}")
                .and_then(|_| input.flush())
                .is_err()
            {
                break;
            }
        }
    });
    let mut received = fs::File::create(dir.join("native-output.jsonl")).unwrap();
    let mut output = std::io::stdout().lock();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        writeln!(received, "{line}").unwrap();
        received.flush().unwrap();
        if writeln!(output, "{line}")
            .and_then(|_| output.flush())
            .is_err()
        {
            break;
        }
    }
    let status = child.wait().unwrap();
    fs::write(
        dir.join("native-exit.json"),
        format!("{{\"code\":{}}}", status.code().unwrap_or(-1)),
    )
    .ok();
    std::process::exit(status.code().unwrap_or(1));
}
