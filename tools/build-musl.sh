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
    LINKER="${CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER:-musl-gcc}"
    ;;
  aarch64-unknown-linux-musl)
    LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER:-aarch64-linux-musl-gcc}"
    ;;
  *)
    echo "unsupported musl target: $TARGET" >&2
    exit 2
    ;;
esac

command -v "$LINKER" >/dev/null || {
  echo "musl linker not found: $LINKER" >&2
  exit 2
}

# Preserve CI's -D warnings or a caller's extra flags while adding the flag
# required for Rust shared libraries on musl.
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=-crt-static"
export CC="${CC:-$LINKER}"
case "$TARGET" in
  x86_64-unknown-linux-musl)
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER"
    ;;
  aarch64-unknown-linux-musl)
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$LINKER"
    ;;
esac

# The native speech archive published by sherpa-onnx is not yet qualified for
# musl. Build the rest of the workspace, then build voice without local-stt so
# the extension still provides the hosted OpenAI-compatible STT path. Package
# backfills use the same rule for a selected rpi-voice release.
if [[ "${PACKAGES_ONLY:-0}" == "1" ]]; then
  : "${PACKAGE_RELEASES:?PACKAGE_RELEASES is required for package-only builds}"
  python3 - "$PACKAGE_RELEASES" <<'PY'
import json
import subprocess
import sys

selected = json.loads(sys.argv[1])
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"
], text=True))
packages = {
    f"{package['name']}-v{package['version']}": package
    for package in metadata["packages"]
    if package["id"] in metadata["workspace_members"]
}
names = [packages[tag]["name"] for tag in selected]
regular = [name for name in names if name != "rpi-voice"]
if regular:
    subprocess.run(["cargo", "build", "--release", "--locked", "--lib", *sum((["-p", name] for name in regular), [])], check=True)
if "rpi-voice" in names:
    subprocess.run(["cargo", "build", "--release", "--locked", "--lib", "--no-default-features", "-p", "rpi-voice"], check=True)
PY
else
  cargo build --release --workspace --locked --exclude rpi-voice
  cargo build --release --locked --no-default-features -p rpi-voice
fi

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
if [[ -f "${RELEASE_DIR}/librpi_goal.so" ]]; then
  if ! nm -D "${RELEASE_DIR}/librpi_goal.so" | grep -q ' rpi_plugin_register$'; then
    echo "rpi-goal does not export rpi_plugin_register" >&2
    exit 1
  fi
  "$CC" tools/musl-plugin-loader.c -o "$RELEASE_DIR/musl-plugin-loader" -ldl
  "$RELEASE_DIR/musl-plugin-loader" "${RELEASE_DIR}/librpi_goal.so"
fi

# With no release metadata requested this is a local build-only helper.
if [[ -z "${SOURCE_REF:-}" ]]; then
  exit 0
fi

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required when packaging}"
: "${PACKAGE_RELEASES:?PACKAGE_RELEASES is required when packaging}"
: "${BUNDLE_TAG:?BUNDLE_TAG is required when packaging}"

if [[ "${PACKAGES_ONLY:-0}" != "1" ]]; then
  cargo test --locked -p rpi-package-install
  cargo run --release --locked -p rpi-package-install -- pack
  cargo run --release --locked -p rpi-package-install -- pack-acp
fi

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
