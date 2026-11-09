//! Test-only launcher adding fixed discovery exclusions to an explicit native.
//! It forwards stdin/stdout unchanged; no protocol responses or events are made.
fn main() {
    let executable =
        std::env::var_os("RPI_CODEMODE_NATIVE_BIN").expect("explicit native executable");
    let status = std::process::Command::new(executable)
        .args([
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
            "--no-prompt-templates",
            "--no-tools",
            "--local-only",
        ])
        .args(std::env::args_os().skip(1))
        .status()
        .expect("native child launch");
    std::process::exit(status.code().unwrap_or(1));
}
