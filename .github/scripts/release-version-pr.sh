#!/usr/bin/env bash
# Merge only the tested release metadata through a PR, never push to main.
set -euo pipefail

base="$(git rev-parse HEAD)"
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Cargo.toml Cargo.lock packages/ catalog/packages.json
if ! git diff --cached --quiet; then
  branch="codex/release-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
  git switch -c "$branch"
  git commit -m "chore(release): update versions and changelogs"
  expected_tree="$(git rev-parse 'HEAD^{tree}')"
  if [[ "$(git ls-remote origin refs/heads/main | cut -f1)" != "$base" ]]; then
    echo "::error::main advanced during validation. Retry the release on the new main."
    exit 1
  fi
  git push origin "HEAD:refs/heads/$branch"
  body="${RUNNER_TEMP}/release-pr-body.md"
  cat > "$body" <<'EOF'
Updates extension versions, changelogs, Cargo.lock and the package catalog for the automatic release.

Validation: workspace tests and Cargo publishing dry-run passed before this PR was created.
EOF
  pr="$(gh pr create --base main --head "$branch" \
    --title 'chore(release): update versions and changelogs' --body-file "$body")"
  if [[ "$(git ls-remote origin refs/heads/main | cut -f1)" != "$base" ]]; then
    echo "::error::main advanced before the version PR could merge. Retry the release."
    exit 1
  fi
  gh pr merge "$pr" --squash --match-head-commit "$(git rev-parse HEAD)"
  merged="$(gh pr view "$pr" --json mergeCommit --jq .mergeCommit.oid)"
  git fetch origin "$merged"
  git checkout --detach "$merged"
  if [[ "$(git rev-parse 'HEAD^{tree}')" != "$expected_tree" ]]; then
    echo "::error::The merged tree changed during validation. Retry before publishing."
    exit 1
  fi
  git push origin --delete "$branch"
fi

echo "source_ref=$(git rev-parse HEAD)" >> "$GITHUB_OUTPUT"
