#!/usr/bin/env bash
# Self-update the skill's knowledge of Mermaid, straight from the authoritative
# source (the mermaid-js/mermaid repo). Keeps the skill at the cutting edge as
# new diagram types and syntax land.
#
# Regenerates:
#   reference/diagram-types.md   — catalog of shipped diagram types
#   reference/syntax/<type>.md   — official per-type syntax docs (offline copy)
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${SKILL_DIR}/reference/diagram-types.md"
SYNTAX_DIR="${SKILL_DIR}/reference/syntax"
API="https://api.github.com/repos/mermaid-js/mermaid"
DIAGRAMS_PATH="packages/mermaid/src/diagrams"
DOCS_PATH="packages/mermaid/src/docs/syntax"

AUTH=()
[ -n "${GITHUB_TOKEN:-}" ] && AUTH=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
[ -z "${GITHUB_TOKEN:-}" ] && [ -n "${GH_TOKEN:-}" ] && AUTH=(-H "Authorization: Bearer ${GH_TOKEN}")

command -v curl >/dev/null 2>&1 || { echo "ERROR: curl required"; exit 1; }
HAVE_JQ=0; command -v jq >/dev/null 2>&1 && HAVE_JQ=1

gh_get() { curl -fsSL ${AUTH[@]+"${AUTH[@]}"} "$1" 2>/dev/null || true; }

# ---------------------------------------------------------------------------
# 1. Catalog: directory names under src/diagrams = shipped diagram types.
# ---------------------------------------------------------------------------
echo "Fetching diagram catalog from ${DIAGRAMS_PATH}..."
DIRS_JSON="$(gh_get "${API}/contents/${DIAGRAMS_PATH}")"
[ -z "${DIRS_JSON}" ] && { echo "ERROR: GitHub API unreachable. Nothing updated."; exit 1; }

if [ "${HAVE_JQ}" = 1 ]; then
  TYPES="$(printf '%s' "${DIRS_JSON}" | jq -r '.[] | select(.type=="dir") | .name' | sort)"
else
  TYPES="$(printf '%s' "${DIRS_JSON}" | grep -o '"name"[^,]*\|"type": *"dir"' \
    | paste - - | grep 'dir' | sed -E 's/.*"name": *"([^"]+)".*/\1/' | sort)"
fi

VERSION="$(curl -fsSL "https://registry.npmjs.org/mermaid/latest" 2>/dev/null \
  | (jq -r '.version' 2>/dev/null || sed -E 's/.*"version": *"([^"]+)".*/\1/;q') || true)"
[ -z "${VERSION}" ] && VERSION="unknown"
TODAY="$(date +%Y-%m-%d)"

# Not authorable diagrams: shared helpers, and the renderer Mermaid uses for syntax errors.
SKIP_TYPES='^(common|error)$'
{
  echo "# Mermaid diagram types (auto-generated)"
  echo
  echo "> Last refreshed: ${TODAY} · Mermaid latest: ${VERSION}"
  echo "> Source of truth: \`${DIAGRAMS_PATH}\` in mermaid-js/mermaid."
  echo "> Regenerate with \`scripts/refresh.sh\`. Per-type syntax is in \`reference/syntax/\`."
  echo "> \`scripts/check.sh\` uses the installed Mermaid CLI, which may be newer than this stamp."
  echo "> \`common\` and \`error\` are omitted: shared code and the syntax-error renderer."
  echo "> Authoring coverage (IR, tests, gaps) is \`reference/coverage.md\`, not this list."
  echo
  echo "## Shipped diagram implementations"
  echo
  echo "The opening keyword may differ from the directory name (e.g."
  echo "\`flowchart\`/\`graph\`, \`stateDiagram-v2\`, \`architecture-beta\`)."
  echo "Keyword, skeleton, and pitfalls: \`reference/type-cheatsheets.md\`."
  echo
  while IFS= read -r t; do
    [ -n "$t" ] || continue
    printf '%s\n' "$t" | grep -Eq "${SKIP_TYPES}" && continue
    echo "- \`$t\`"
  done <<< "${TYPES}"
} > "${OUT}"
echo "Wrote ${OUT}"

# ---------------------------------------------------------------------------
# 2. Offline copy of official per-type syntax docs.
# ---------------------------------------------------------------------------
echo "Caching official syntax docs from ${DOCS_PATH}..."
mkdir -p "${SYNTAX_DIR}"
DOCS_JSON="$(gh_get "${API}/contents/${DOCS_PATH}")"
if [ -z "${DOCS_JSON}" ]; then
  echo "WARN: could not list syntax docs; kept existing cache."
else
  if [ "${HAVE_JQ}" = 1 ]; then
    PAIRS="$(printf '%s' "${DOCS_JSON}" \
      | jq -r '.[] | select(.type=="file" and (.name|endswith(".md"))) | "\(.name)\t\(.download_url)"')"
  else
    # Fallback: reconstruct raw URLs from names on the default branch.
    BRANCH="$(gh_get "${API}" | (jq -r '.default_branch' 2>/dev/null || sed -E 's/.*"default_branch": *"([^"]+)".*/\1/;q'))"
    [ -z "${BRANCH}" ] && BRANCH="develop"
    NAMES="$(printf '%s' "${DOCS_JSON}" | grep -o '"name": *"[^"]*\.md"' | sed -E 's/.*"([^"]+)"/\1/')"
    PAIRS="$(while IFS= read -r n; do [ -n "$n" ] && printf '%s\thttps://raw.githubusercontent.com/mermaid-js/mermaid/%s/%s/%s\n' "$n" "$BRANCH" "$DOCS_PATH" "$n"; done <<< "$NAMES")"
  fi

  COUNT=0
  while IFS=$'\t' read -r name url; do
    [ -z "$name" ] || [ -z "$url" ] && continue
    if curl -fsSL "$url" -o "${SYNTAX_DIR}/${name}" 2>/dev/null; then
      COUNT=$((COUNT+1))
    fi
  done <<< "${PAIRS}"

  {
    echo "# Cached Mermaid syntax docs"
    echo
    echo "Auto-downloaded ${TODAY} (Mermaid ${VERSION}) from mermaid-js/mermaid \`${DOCS_PATH}\`."
    echo "Regenerate with \`scripts/refresh.sh\`. One file per diagram type."
    echo "Open a file only when the cheatsheet in \`../type-cheatsheets.md\` is not enough."
    echo "Which types have an IR is \`../coverage.md\`."
  } > "${SYNTAX_DIR}/README.md"
  echo "Cached ${COUNT} syntax docs into ${SYNTAX_DIR}"
fi

echo "Done. Types:"
printf '%s\n' "${TYPES}" | sed 's/^/  - /'
