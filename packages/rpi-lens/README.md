# rpi-lens

A bounded diagnostics extension inspired by `pi-lens`. The `code_lens` tool
runs one allowlisted check: `git diff --check`, `cargo check`,
`cargo fmt --check`, or `cargo clippy -- -D warnings`. Commands time out after
at most 120 seconds and output is capped for agent context safety.
