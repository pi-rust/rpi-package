#!/usr/bin/env bash
# Validate/render a Mermaid diagram LOCALLY, without the MCP server.
# Uses @mermaid-js/mermaid-cli (mmdc). Good as a fallback when the MCP is
# unavailable, or for offline/CI validation.
#
# Usage:
#   check.sh diagram.mmd                 # validate (render to a temp SVG, discard)
#   check.sh diagram.mmd -o out.svg      # validate AND keep the rendered SVG/PNG/PDF
#   cat diagram.mmd | check.sh           # read from stdin
#   check.sh -c 'flowchart TD; A-->B'    # inline code
#
# Exit 0 = renders cleanly. Non-zero = syntax/render error (message printed).
#
# Optional: set MERMAID_PUPPETEER_CONFIG to a puppeteer JSON config path (e.g.
# {"args": ["--no-sandbox"]}) when the host can't grant Chromium a sandbox — some CI runners
# (e.g. GitHub Actions' ubuntu-latest, whose AppArmor policy blocks unprivileged user
# namespaces) need this. Unset by default; local/interactive use is unaffected.
set -euo pipefail

OUT=""; INPUT=""; CODE=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) OUT="${2:-}"; shift 2;;
    -c) CODE="${2:-}"; shift 2;;
    -h|--help) sed -n '2,11p' "$0"; exit 0;;
    *) INPUT="$1"; shift;;
  esac
done

TMPDIR_LOCAL="$(mktemp -d)"
trap 'rm -rf "${TMPDIR_LOCAL}"' EXIT

# Resolve the source into a .mmd file.
SRC="${TMPDIR_LOCAL}/in.mmd"
if [ -n "${CODE}" ]; then
  printf '%s\n' "${CODE}" > "${SRC}"
elif [ -n "${INPUT}" ]; then
  [ -f "${INPUT}" ] || { echo "ERROR: file not found: ${INPUT}"; exit 2; }
  cp "${INPUT}" "${SRC}"
elif [ ! -t 0 ]; then
  cat > "${SRC}"
else
  echo "ERROR: no input. Pass a file, -c '<code>', or pipe via stdin. (-h for help)"; exit 2
fi

[ -s "${SRC}" ] || { echo "ERROR: empty diagram."; exit 2; }

# Find a mermaid CLI: prefer an installed mmdc, else a pinned npx package.
# Keep this pin in sync with .github/workflows/tests.yml. Mermaid 11 CLIs do not
# register agentflow-beta or usecase-beta (UnknownDiagramError). mermaid-cli 12
# requires Node >= 22.13.
MERMAID_CLI_PACKAGE="${MERMAID_CLI_PACKAGE:-@mermaid-js/mermaid-cli@12.0.0}"
if command -v mmdc >/dev/null 2>&1; then
  RUN=(mmdc)
elif command -v npx >/dev/null 2>&1; then
  echo "note: mmdc not installed; using 'npx ${MERMAID_CLI_PACKAGE}' (first run downloads it)."
  RUN=(npx -y "${MERMAID_CLI_PACKAGE}")
else
  echo "ERROR: need either 'mmdc' or 'npx' on PATH."
  echo "Install once with: npm install -g @mermaid-js/mermaid-cli"
  exit 3
fi

VALIDATION_DEST="${TMPDIR_LOCAL}/validation.svg"
ERRLOG="${TMPDIR_LOCAL}/err.log"

# Portable helper (avoids expanding a possibly-empty array under `set -u`, which throws
# "unbound variable" on bash 3.2 — macOS's default /usr/bin/bash — even though bash 4.4+
# handles it fine).
run_mmdc() {
  if [ -n "${MERMAID_PUPPETEER_CONFIG:-}" ]; then
    "${RUN[@]}" -i "${SRC}" -o "$1" -p "${MERMAID_PUPPETEER_CONFIG}"
  else
    "${RUN[@]}" -i "${SRC}" -o "$1"
  fi
}

if ! run_mmdc "${VALIDATION_DEST}" >"${ERRLOG}" 2>&1; then
  echo "FAIL: Mermaid could not render the diagram."
  echo "----- mermaid-cli output -----"
  cat "${ERRLOG}"
  echo "------------------------------"
  echo "Fix the reported line/token and re-run. Syntax reference:"
  echo "  reference/syntax/<type>.md  or  reference/type-cheatsheets.md"
  exit 1
fi

# Mermaid CLI can exit 0 while writing its syntax-error placeholder SVG. Inspect
# the artifact itself so an error image is never reported as a successful render.
if grep -Eiq 'aria-roledescription="error"|class="error-icon"|>Syntax error in text<' "${VALIDATION_DEST}"; then
  echo "FAIL: Mermaid emitted an error SVG despite returning exit 0."
  echo "----- mermaid-cli output -----"
  cat "${ERRLOG}"
  echo "------------------------------"
  echo "Inspect the source with the target Mermaid version; beta diagram lexers may reject"
  echo "label punctuation accepted by other diagram types."
  exit 1
fi

if [ -n "${OUT}" ]; then
  case "${OUT}" in
    *.svg) cp "${VALIDATION_DEST}" "${OUT}" ;;
    *)
      if ! run_mmdc "${OUT}" >"${ERRLOG}" 2>&1; then
        echo "FAIL: Mermaid validated as SVG but could not write ${OUT}."
        cat "${ERRLOG}"
        exit 1
      fi
      ;;
  esac
fi

echo "PASS: diagram renders cleanly."
[ -n "${OUT}" ] && echo "Wrote ${OUT}"
exit 0
