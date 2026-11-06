# Prebuilt extension distribution protocol v1

This protocol maps an extension's Cargo package name and **package version** to
its prebuilt native library. A repository may publish many extensions with
independent versions; the workspace version and latest repository Release are
not substitutes for the requested package version.

## Release workflow

Every push or PR merge to `main` runs
`.github/workflows/release-cargo.yml`. release-plz compares the workspace with
crates.io, updates versions and package changelogs for unpublished changes,
and synchronizes `catalog/packages.json`. Ordinary changes to the current
`0.x` packages get a patch bump; conventional breaking-change commits can
increase the minor version. Packages inheriting the workspace version share
that version. Already prepared, unpublished versions are preserved.
Release analysis includes history after the migration to registry SDK
dependencies; older commits requiring an external `pi-rust` checkout are
excluded in the CI checkout so historical package comparisons can run.

The workflow tests the workspace and dry-runs Cargo publishing before creating
and merging a version PR. It respects protected `main` without directly pushing
to it. It then publishes unpublished extension crates and creates per-package
tags (`<package>-v<version>`) and GitHub Releases.
The installer is excluded because it has `publish = false`. Publishing runs
in the same workflow as the version update. Runs are serialized, and a failed
release can be retried from the Actions UI or with `workflow_dispatch` on `main`.

Configure the repository Actions secret `CARGO_REGISTRY_TOKEN` with crates.io
publishing permission for all extension crates, including permission to create
any names that have not been published yet. The workflow's `GITHUB_TOKEN` needs
permission to create and merge the version PR and create tags/releases. Enable
Actions PR creation in the repository settings. If branch rules require
approvals or checks, the version PR must satisfy them before releasing.
If organization policy blocks Actions PR creation, set the optional
`RELEASE_GITHUB_TOKEN` secret with repository Contents and Pull requests write
permissions instead.

After crate publishing, `release-assets.yml` automatically builds Windows x64,
Linux x64/ARM64 GNU, Linux x64 musl, and macOS Intel/Apple Silicon binaries
from the exact release commit. Each package Release receives individual
extension archives and an `rpi-extension.json` manifest with download URLs,
SHA-256 hashes, target triples, library names and SDK ABI. A separate bundle
Release contains the catalog collection, installer and standalone ACP bridge;
archived host-owned packages are intentionally omitted. The existing
`release-unix.yml` manual entry point uses the same target set.

Host developers can implement prebuilt installation using the discovery
protocol below and its [JSON schema](rpi-extension.schema.json). Discover the
repository from crates.io metadata, then use the requested package's versioned Release manifest;
packages in this repository do not all share one version.

## Discovery

1. Resolve the package and requested version through crates.io. With no version
   specified, select the latest non-yanked stable version. Use the package's
   `repository` metadata to locate the GitHub repository; do not infer an owner
   from the package name. The official package catalog also includes
   `repository` and `prebuilt_manifest` for each recorded version.
   For example, `GET https://crates.io/api/v1/crates/rpi-goal` exposes
   `crate.repository` as `https://github.com/pi-rust/rpi-package` and provides
   versions in `versions[].num` with their `yanked` flags.
2. For a repository at `https://github.com/<owner>/<repo>`, request:

   ```text
   https://github.com/<owner>/<repo>/releases/download/<package>-v<version>/rpi-extension.json
   ```

3. Require `schema_version == 1` and check that `name`, `version`, `repository`
   and `tag` match the resolved package. Match an exact Rust target triple from
   `targets`, including the libc/ABI variant. GNU Linux archives cannot be
   substituted for musl, Windows MSVC for GNU, or x86_64 for aarch64.
4. Check the recorded plugin ABI against the host's supported ABI. `sdk.version`
   is the actual SDK dependency compiled into the library, not the manifest's
   minimum version requirement. The host's existing entrypoint validation must
   also check ABI and API struct size when loading the extension.
5. Download the selected archive, check its byte length and SHA-256, and
   extract only the declared library. Check the extracted library's SHA-256,
   then install it using the host's normal transactional extension installer.

The distribution is source-agnostic once discovered: multiple packages can
resolve to the same repository, and third-party repositories can implement the
same protocol. A host may provide an explicit repository override or trusted
registry mapping for extensions that have not been published to crates.io.

`rpi-extension.json` is uploaded **after all platform archives are available**.
A missing manifest means the requested version has no complete prebuilt release
yet. Report that state or retry later; never silently install another package's
latest bundle, downgrade the version, or compile source without the user's
selected fallback policy. Checksums provide download integrity; the repository
selected during discovery remains the publisher trust boundary.

## Package Release layout

Example for `rpi-goal` version `0.1.7` in this repository:

```text
Release tag: rpi-goal-v0.1.7
  rpi-extension.json
  rpi-goal-0.1.7-x86_64-pc-windows-msvc.zip
  rpi-goal-0.1.7-x86_64-unknown-linux-gnu.tar.gz
  rpi-goal-0.1.7-aarch64-unknown-linux-gnu.tar.gz
  rpi-goal-0.1.7-x86_64-unknown-linux-musl.tar.gz
  rpi-goal-0.1.7-x86_64-apple-darwin.tar.gz
  rpi-goal-0.1.7-aarch64-apple-darwin.tar.gz
```

Each package archive is flat and contains only that extension's library. Cargo
library target names determine file names, including `lib` on Unix; consumers
must read `library.path` rather than derive it from the package name.

Example manifest showing one target (real releases include all six supported targets):

```json
{
  "schema_version": 1,
  "name": "rpi-goal",
  "version": "0.1.7",
  "repository": "https://github.com/pi-rust/rpi-package",
  "tag": "rpi-goal-v0.1.7",
  "source_commit": "0123456789abcdef0123456789abcdef01234567",
  "sdk": { "version": "0.3.20", "plugin_abi": 4 },
  "targets": {
    "x86_64-pc-windows-msvc": {
      "url": "https://github.com/pi-rust/rpi-package/releases/download/rpi-goal-v0.1.7/rpi-goal-0.1.7-x86_64-pc-windows-msvc.zip",
      "format": "zip",
      "size": 123456,
      "sha256": "<64 lowercase hexadecimal characters>",
      "library": {
        "path": "rpi_goal.dll",
        "sha256": "<64 lowercase hexadecimal characters>"
      }
    }
  }
}
```

The machine-readable contract is [rpi-extension.schema.json](rpi-extension.schema.json).
Unknown fields may be ignored; unknown schema versions must be rejected.

## Bundles and build provenance

The aggregate Release `rpi-packages-<first 12 characters of source commit>`
contains `rpi-packages.json`, six full platform bundles, and six standalone
ACP bridge archives. Bundles contain all workspace cdylibs, the installer, and
the catalog. `rpi-packages.json` maps target triples to archive checksums and
the individual package versions, SDK identities and library checksums.
It is a convenience distribution for installing the whole collection; single
package installation uses the package Release above.

All six builds use the same exact Git commit. Linux GNU builds use Ubuntu
22.04; the x86_64 musl build uses a native musl toolchain container. Normal
runtime dependencies such as glibc/musl, ALSA and OpenSSL still apply. The
supported targets are Windows x64 MSVC, Linux x64/ARM64 GNU, Linux x64 musl,
and macOS Intel/Apple Silicon. Windows ARM64, Linux aarch64 musl and 32-bit
platforms are not currently built. The loader performs the final host
compatibility check.

Musl builds use `-C target-feature=-crt-static` because Rust otherwise drops
`cdylib` targets for the musl target. The `rpi-voice` musl artifact is built
without its default `local-stt` feature until a musl-compatible sherpa-onnx
native archive is qualified; hosted OpenAI-compatible STT remains available.
Do not mix GNU and musl plugins or hosts: the libc variant is part of the
exact target triple and the archive slug.

## Release automation

On a push to `main`, the release workflow prepares versions and changelogs,
synchronizes the catalog, runs tests and Cargo publishing preflight, and opens
and squash-merges a PR containing the tested release metadata. It never pushes
directly to protected `main`. The repository must allow Actions to create PRs;
the version PR is merged only when branch protection permits it. If approval or
required checks are added later, merge that PR through the normal review flow
and rerun the release workflow on `main`.

The workflow then publishes crates, creates per-package tags/Releases, and
calls the cross-platform build workflow directly. This direct call is needed
because pushes/releases made with `GITHUB_TOKEN` do not trigger another
workflow automatically (see [GitHub's token event behavior](https://docs.github.com/en/actions/concepts/security/github_token)).
When organization policy permits Actions PR creation,
the built-in token is sufficient. Otherwise set `RELEASE_GITHUB_TOKEN` to a
repository-scoped GitHub App token or fine-grained personal access token with
Contents and Pull requests write permissions. This token is used for the PR
API only; it does not bypass `main` protection. A version-PR push triggered by
that token is skipped by the release workflow because the original run already
publishes the merged commit. `workflow_dispatch` can always retry that commit.

The build pipeline preserves Actions artifacts for 14 days. Six-platform
assembly verifies source commit, package identity, SDK identity and checksums
before uploading discovery manifests. The musl build also uses a native musl
`dlopen`/`dlsym` smoke loader against `rpi-goal` before packaging. A completed
manifest and its archives are preserved on retry; unfinished releases can
finish uploading safely.
To recover a failed asset build, rerun the failed jobs or manually invoke
`release-assets.yml` with the published commit and a JSON array of its package
tags. The existing `release-unix.yml` entry point now manually builds all six
platform bundles from its supplied tag.

Repository prerequisites: `CARGO_REGISTRY_TOKEN` with publishing permission
for all extension crates, Actions PR creation enabled or `RELEASE_GITHUB_TOKEN`,
and permission for the workflow token to push its release branch and create
Releases. `main` remains protected against direct pushes, including bot pushes.

## Optional RustFS/CDN mirror

GitHub Releases remain the canonical package source. The asset workflow can
also copy the exact validated archives and discovery manifests to an optional
S3-compatible RustFS bucket. Configure these repository Actions secrets to
enable it:

```text
RPI_RUSTFS_ENDPOINT
RPI_RUSTFS_BUCKET
RPI_RUSTFS_ACCESS_KEY
RPI_RUSTFS_SECRET_KEY
RPI_CDN_BASE_URL=https://assets.bigfisharc.cn
```

Objects use the shared public layout:

```text
rpi-package/releases/<package-tag>/rpi-extension.json
rpi-package/releases/<package-tag>/<archive>
rpi-package/releases/<bundle-tag>/rpi-packages.json
rpi-package/releases/<bundle-tag>/<archive>
```

The mirror is skipped when no RustFS settings are present. A partial
configuration fails clearly rather than silently publishing an incomplete
mirror. Existing objects are checked for size and the recorded SHA-256 before
they are preserved; versioned objects are never overwritten with different
content. Configure the RustFS bucket/CDN for public object reads without
public listing, writing or deleting, and use a short cache for manifests and
long immutable caching for versioned archives. The client continues to trust
the GitHub manifest and uses the CDN only as an optional archive accelerator.
