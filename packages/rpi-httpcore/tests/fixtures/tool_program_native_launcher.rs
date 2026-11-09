//! Test-only fixed-discovery pipe forwarder. Never fabricates native replies.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let dir = args
        .windows(2)
        .find(|p| p[0] == "--session-dir")
        .map(|p| std::path::PathBuf::from(&p[1]))
        .expect("explicit session directory");
    let evidence = dir.parent().unwrap();
    let mut child =
        Command::new(std::env::var_os("RPI_CODEMODE_NATIVE_BIN").expect("explicit frozen native"))
            .args([
                "--no-extensions",
                "--no-skills",
                "--no-context-files",
                "--no-prompt-templates",
                "--no-tools",
                "--local-only",
            ])
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("native launch");
    fs::write(evidence.join("native-pid.txt"), child.id().to_string()).unwrap();
    fs::write(evidence.join("launcher-args.txt"), format!("{args:?}")).unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut sent = fs::File::create(evidence.join("native-input.jsonl")).unwrap();
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
    let mut received = fs::File::create(evidence.join("native-output.jsonl")).unwrap();
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
        evidence.join("native-exit.json"),
        format!("{{\"code\":{}}}", status.code().unwrap_or(-1)),
    )
    .ok();
    std::process::exit(status.code().unwrap_or(1));
}
