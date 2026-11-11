# Release workflow backup — 2026-11-10

This directory preserves the release workflow definitions that were active on `main` before the simplified build pipeline was restored.

- `release-cargo.yml`: cargo release orchestration
- `release-assets.yml`: cross-platform extension bundle and package assets
- `release-web.yml`: portable Web releases
- `release-packages.yml`: package-only backfill
- `release-unix.yml`: Unix release helper

The active `release-assets.yml` and `release-web.yml` now use the initial simple build paths from commits `01649f4` and `18864f4`, while retaining the current backfill/publication plumbing, reusable-workflow secret declarations, strict `RUSTFLAGS=-D warnings`, and supported macOS ARM runner.

Intentional cost/release trade-offs:

- the x86_64 musl extension build remains enabled; the portable Web musl binary build also remains enabled;
- no native speech cache reset/restore in the asset matrix; the original package-install test remains;
- the non-musl matrix uses the simple Web binary build and does not build the later Web prebuilt extension artifacts;
- Web builds package the original Web release archive only for GNU targets, without the later GNU Web prebuilt extension artifacts; the musl Web binary and RustFS mirroring remain enabled;
- the original full workspace build and simple bundle packaging remain enabled.

Restore the backed-up files to return to the previous pipeline.
