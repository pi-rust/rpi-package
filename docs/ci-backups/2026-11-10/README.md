# Release workflow backup — 2026-11-10

This directory preserves the release workflow definitions that were active on `main` before the simplified build pipeline was restored.

- `release-cargo.yml`: cargo release orchestration
- `release-assets.yml`: cross-platform extension bundle and package assets
- `release-web.yml`: portable Web releases
- `release-packages.yml`: package-only backfill
- `release-unix.yml`: Unix release helper

The active `release-assets.yml` and `release-web.yml` now use the initial simple build paths from commits `01649f4` and `18864f4`, while retaining the current backfill/publication plumbing, reusable-workflow secret declarations, strict `RUSTFLAGS=-D warnings`, and supported macOS ARM runner.

Intentional cost/release trade-offs:

- no separate x86_64 musl extension build;
- no native speech cache reset/restore in the asset matrix; the original package-install test remains;
- no Web musl build;
- Web builds package the original Web release archive only, without the later prebuilt Web extension artifacts; RustFS mirroring for the Web archive remains enabled;
- the original full workspace build and simple bundle packaging remain enabled.

Restore the backed-up files to return to the previous pipeline.
