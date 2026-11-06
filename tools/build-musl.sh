#!/usr/bin/env bash
# Build rpi extension libraries in a native musl environment.
#
# A musl target defaults to a static CRT, and Rust consequently drops cdylib
# targets. Use a native musl compiler and build only the combinations declared
# by each crate. Voice is excluded by its package-owned support declaration.
set -euo pipefail

ROOT_DIR="${PROJECT_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SCRIPT_ROOT="${SCRIPT_ROOT:-$ROOT_DIR}"
cd "$ROOT_DIR"

TARGET="${TARGET:-x86_64-unknown-linux-musl}"
case "$TARGET" in
  x86_64-unknown-linux-musl)
    LINKER="${CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER:-}"
    if [[ -z "$LINKER" ]]; then
      for candidate in x86_64-unknown-linux-musl-gcc x86_64-linux-musl-gcc musl-gcc cc; do
        if command -v "$candidate" >/dev/null; then
          LINKER="$candidate"
          break
        fi
      done
    fi
    ;;
  aarch64-unknown-linux-musl)
    LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER:-aarch64-linux-musl-gcc}"
    ;;
  *)
    echo "unsupported musl target: $TARGET" >&2
    exit 2
    ;;
esac

if [[ -z "$LINKER" ]] || ! command -v "$LINKER" >/dev/null; then
  echo "musl linker not found: ${LINKER:-x86_64-unknown-linux-musl-gcc or musl-gcc}" >&2
  exit 2
fi

COMPILER_HOST="$(rustc -vV | sed -n 's/^host: //p')"
if [[ "$COMPILER_HOST" != "$TARGET" ]]; then
  echo "musl releases require a native $TARGET compiler, got $COMPILER_HOST" >&2
  exit 2
fi

# Preserve CI's -D warnings or a caller's extra flags while adding the flag
# required for Rust shared libraries on musl.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=-crt-static"
export CC="${CC:-$LINKER}"
# Keep target-qualified C/AR invocations on the native musl toolchain.
case "$TARGET" in
  x86_64-unknown-linux-musl)
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER"
    export CC_x86_64_unknown_linux_musl="$LINKER"
    export AR_x86_64_unknown_linux_musl="${AR:-ar}"
    ;;
  aarch64-unknown-linux-musl)
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER"
    export CC_aarch64_unknown_linux_musl="$LINKER"
    export AR_aarch64_unknown_linux_musl="${AR:-ar}"
    ;;
esac

# Resolve the crate-owned target declarations before compiling. Unsupported
# packages are intentionally absent from the musl build; they are not failed
# builds. A marker keeps the Actions artifact non-empty for package backfills
# containing only GNU-only releases. Manual bundle-only runs have no package
# tags and skip musl entirely because the aggregate bundle is GNU-only.
if [[ -n "${SOURCE_REF:-}" && "${PACKAGES_ONLY:-0}" != "1" && "${PACKAGE_RELEASES:-[]}" == "[]" ]]; then
  mkdir -p dist/release-assets
  printf 'No package releases were selected for the musl target.\n' > dist/release-assets/.no-musl-packages
  exit 0
fi
if [[ "${PACKAGES_ONLY:-0}" == "1" ]]; then
  : "${PACKAGE_RELEASES:?PACKAGE_RELEASES is required for package-only builds}"
fi
MUSL_PACKAGE_NAMES="$(python3 - "${PACKAGE_RELEASES:-[]}" "${PACKAGES_ONLY:-0}" <<'PY'
import json
import subprocess
import sys

selected = set(json.loads(sys.argv[1]))
packages_only = sys.argv[2] == "1"
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"
], text=True))
workspace = set(metadata["workspace_members"])
for package in metadata["packages"]:
    if package["id"] not in workspace:
        continue
    if not any("cdylib" in target.get("crate_types", []) for target in package["targets"]):
        continue
    package_metadata = package.get("metadata") or {}
    rpi_metadata = package_metadata.get("rpi") or {}
    prebuilt = rpi_metadata.get("prebuilt") or {}
    declared = prebuilt.get("supported-targets", []) if isinstance(prebuilt, dict) else []
    if "x86_64-unknown-linux-musl" not in declared:
        continue
    tag = f"{package['name']}-v{package['version']}"
    if packages_only and tag not in selected:
        continue
    print(package["name"])
PY
)"

if [[ -z "$MUSL_PACKAGE_NAMES" ]]; then
  mkdir -p dist/release-assets
  printf 'No selected workspace packages declare x86_64-unknown-linux-musl support.\n' > dist/release-assets/.no-musl-packages
  exit 0
fi

mapfile -t MUSL_PACKAGES <<< "$MUSL_PACKAGE_NAMES"
cargo_args=(build --release --locked --lib)
for package in "${MUSL_PACKAGES[@]}"; do
  cargo_args+=(--package "$package")
done
cargo "${cargo_args[@]}"

# Cargo can exit successfully after dropping an unsupported cdylib, and a
# glibc-linked object can look superficially like a musl build. Fail closed on
# both cases before anything is packaged.
RELEASE_DIR="$(cargo metadata --no-deps --format-version 1 --locked | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release"
mapfile -t LIBRARIES < <(python3 - "$RELEASE_DIR" "${MUSL_PACKAGES[@]}" <<'PY'
import json
from pathlib import Path
import subprocess
import sys

metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"
], text=True))
selected = set(sys.argv[2:])
for package in metadata["packages"]:
    if package["name"] in selected:
        target = next(t for t in package["targets"] if "cdylib" in t["crate_types"])
        print(Path(sys.argv[1]) / ("lib" + target["name"] + ".so"))
PY
)
if [[ "${#LIBRARIES[@]}" -eq 0 ]]; then
  echo "no musl cdylib artifacts found in $RELEASE_DIR" >&2
  exit 1
fi
for library in "${LIBRARIES[@]}"; do
  if [[ ! -f "$library" ]]; then
    echo "missing selected musl library: $library" >&2
    exit 1
  fi
  if readelf -d "$library" | grep -Eq 'libc\.so\.6|ld-linux[^ ]*\.so'; then
    echo "glibc dependency found in musl artifact: $library" >&2
    exit 1
  fi
done
"$CC" tools/musl-plugin-loader.c -o "$RELEASE_DIR/musl-plugin-loader" -ldl
for library in "${LIBRARIES[@]}"; do
  if ! nm -D "$library" | grep -q ' rpi_plugin_register$'; then
    echo "plugin does not export rpi_plugin_register: $library" >&2
    exit 1
  fi
  "$RELEASE_DIR/musl-plugin-loader" "$library"
done

# With no release metadata requested this is a local build-only helper.
if [[ -z "${SOURCE_REF:-}" ]]; then
  exit 0
fi

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required when packaging}"
: "${PACKAGE_RELEASES:?PACKAGE_RELEASES is required when packaging}"
: "${BUNDLE_TAG:?BUNDLE_TAG is required when packaging}"

# Musl currently publishes individual package archives only. The aggregate
# installer bundle remains GNU-only until its catalog can carry the same
# package-specific target matrix.
package_args=()
if [[ "${PACKAGES_ONLY:-0}" == "1" ]]; then
  package_args+=(--packages-only)
fi
python3 "$SCRIPT_ROOT/.github/scripts/prebuilt-assets.py" build \
  "${package_args[@]}" \
  --target "$TARGET" \
  --repository "$GITHUB_REPOSITORY" \
  --source-ref "$SOURCE_REF" \
  --bundle-tag "$BUNDLE_TAG" \
  --tags "$PACKAGE_RELEASES"
