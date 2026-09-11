#!/usr/bin/env bash
set -euo pipefail

workspace_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source_dir="${RPI_PACKAGE_SOURCE_DIR:-$workspace_dir}"
target_dir="${RPI_CODING_AGENT_DIR:+$RPI_CODING_AGENT_DIR/extensions}"
target_dir="${target_dir:-${HOME}/.rpi/agent/extensions}"

mkdir -p "$target_dir"
shopt -s nullglob
artifacts=("$source_dir"/*.so "$source_dir"/*.dylib)
if ((${#artifacts[@]} == 0)); then
  echo "No Linux/macOS extension libraries found in $source_dir" >&2
  exit 1
fi

cp -f "${artifacts[@]}" "$target_dir/"
echo "Installed ${#artifacts[@]} rpi packages to $target_dir"
