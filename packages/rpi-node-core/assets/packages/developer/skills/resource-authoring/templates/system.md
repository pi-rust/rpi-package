# Canonical native resources
Package: prompts/system.md for explicitly mapped persistent instructions; skills/<name>/SKILL.md and references/templates beneath it; optionally extensions/<platform-library> for a separately authorized, built native extension. Resource-only Packages need no Cargo.toml.
Skill: SKILL.md with YAML name and description frontmatter and nonempty instructions; references/ and examples/ contain supporting text.
Extension: native platform cdylib, built separately against the actual compatible SDK. Rust source or extension.design.json is not a loadable library. Static validation makes no ABI or safety guarantee; Work does not run a compiler from a document.
Use only relative source paths. No executable path, argv, environment or browser-selected DLL path is a session configuration reference. Install/select through node APIs using native receipts and opaque references. Tools policies are default, only(names), or none; none must never expand to all.
