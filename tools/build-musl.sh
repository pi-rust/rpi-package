#!/usr/bin/env bash
# Build rpi extension libraries in a native musl environment.
#
# A musl target defaults to a static CRT, and Rust consequently drops cdylib
# targets. This script keeps the required linker/CRT settings together and
# makes the musl voice build explicit: the offline sherpa-onnx feature is not
# assumed to have a musl prebuilt archive, so musl releases build voice in API
# mode unless a separately validated native speech toolchain is supplied.
set -euo pipefail

ROOT_DIR="${PROJECT_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SCRIPT_ROOT="${SCRIPT_ROOT:-$ROOT_DIR}"
cd "$ROOT_DIR"

TARGET="${TARGET:-x86_64-unknown-linux-musl}"
case "$TARGET" in
  x86_64-unknown-linux-musl)
    LINKER="${CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER:-}"
    if [[ -z "$LINKER" ]]; then
      for candidate in x86_64-unknown-linux-musl-gcc x86_64-linux-musl-gcc musl-gcc; do
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

# Preserve CI's -D warnings or a caller's extra flags while adding the flag
# required for Rust shared libraries on musl.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=-crt-static"
export CC="${CC:-$LINKER}"
# Native-tls's vendored OpenSSL build uses target-qualified C tool variables.
# Keep all C/AR invocations on the musl cross toolchain, never the Ubuntu host.
case "$TARGET" in
  x86_64-unknown-linux-musl)
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER"
    export CC_x86_64_unknown_linux_musl="$LINKER"
    export AR_x86_64_unknown_linux_musl="${LINKER%-gcc}-ar"
    ;;
  aarch64-unknown-linux-musl)
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER"
    export CC_aarch64_unknown_linux_musl="$LINKER"
    export AR_aarch64_unknown_linux_musl="${LINKER%-gcc}-ar"
    ;;
esac

# Resolve the crate-owned target declarations before compiling. Unsupported
# packages are intentionally absent from the musl build; they are not failed
# builds. A marker keeps the Actions artifact non-empty for package backfills
# containing only GNU-only releases. Manual bundle-only runs have no package
# tags and skip musl entirely because the aggregate bundle is GNU-only.
if [[ "${PACKAGES_ONLY:-0}" != "1" && "${PACKAGE_RELEASES:-[]}" == "[]" ]]; then
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

read -r -a MUSL_PACKAGES <<< "$MUSL_PACKAGE_NAMES"
cargo_args=(build --release --locked --lib)
for package in "${MUSL_PACKAGES[@]}"; do
  cargo_args+=(--package "$package")
done
cargo "${cargo_args[@]}"

# Cargo can exit successfully after dropping an unsupported cdylib, and a
# glibc-linked object can look superficially like a musl build. Fail closed on
# both cases before anything is packaged.
RELEASE_DIR="target/$TARGET/release"
mapfile -t LIBRARIES < <(find "$RELEASE_DIR" -maxdepth 1 -type f -name '*.so' -print)
if [[ "${#LIBRARIES[@]}" -eq 0 ]]; then
  echo "no musl cdylib artifacts found in $RELEASE_DIR" >&2
  exit 1
fi
for library in "${LIBRARIES[@]}"; do
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
