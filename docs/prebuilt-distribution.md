# Prebuilt extension distribution protocol v1

This protocol maps an extension's Cargo package name and **package version** to
its prebuilt native library. A repository may publish many extensions with
independent versions; the workspace version and latest repository Release are
not substitutes for the requested package version.

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
  rpi-goal-0.1.7-x86_64-apple-darwin.tar.gz
  rpi-goal-0.1.7-aarch64-apple-darwin.tar.gz
```

Each package archive is flat and contains only that extension's library. Cargo
library target names determine file names, including `lib` on Unix; consumers
must read `library.path` rather than derive it from the package name.

Example manifest showing one target (real releases include all five):

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
contains `rpi-packages.json`, five full platform bundles, and five standalone
ACP bridge archives. Bundles contain all workspace cdylibs, the installer, and
the catalog. `rpi-packages.json` maps target triples to archive checksums and
the individual package versions, SDK identities and library checksums.
It is a convenience distribution for installing the whole collection; single
package installation uses the package Release above.

All five builds use the same exact Git commit. Linux GNU builds use Ubuntu
22.04; normal runtime dependencies such as glibc, ALSA and OpenSSL still apply.
The supported targets are Windows x64 MSVC, Linux x64/ARM64 GNU, and macOS
Intel/Apple Silicon. Windows ARM64, Linux musl and 32-bit platforms are not
currently built. The loader performs the final host compatibility check.

## Release automation

Only an explicitly requested manual run on `main` starts a release; pushes and
PR merges do not publish. The release workflow prepares versions and changelogs,
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
API only; it does not bypass `main` protection. Version-PR pushes do not trigger
publication. The explicitly started run publishes the merged commit;
`workflow_dispatch` can retry that commit when requested.

The build pipeline preserves Actions artifacts for 14 days. Five-platform
assembly verifies source commit, package identity, SDK identity and checksums
before uploading discovery manifests. A completed manifest and its archives
are preserved on retry; unfinished releases can finish uploading safely.
To recover a failed asset build, rerun the failed jobs or manually invoke
`release-assets.yml` with the published commit and a JSON array of its package
tags. The existing `release-unix.yml` entry point now manually builds all five
platform bundles from its supplied tag.

Repository prerequisites: `CARGO_REGISTRY_TOKEN` with publishing permission
for all extension crates, Actions PR creation enabled or `RELEASE_GITHUB_TOKEN`,
and permission for the workflow token to push its release branch and create
Releases. `main` remains protected against direct pushes, including bot pushes.
