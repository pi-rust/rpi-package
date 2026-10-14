"""Exercise the version PR script using a local remote and a fake GitHub API."""

import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


def git(directory, *args):
    return subprocess.check_output(["git", "-C", str(directory), *args], encoding="utf-8", stderr=subprocess.PIPE).strip()


def shell_path(path):
    value = pathlib.Path(path).resolve().as_posix()
    return "/" + value[0].lower() + value[2:] if os.name == "nt" else value


@unittest.skipUnless(shutil.which("git") and shutil.which("bash"), "git and bash required")
class ReleaseVersionPrTests(unittest.TestCase):
    def test_merges_metadata_through_a_pr_and_checks_out_the_merged_commit(self):
        with tempfile.TemporaryDirectory(prefix="rpi-release-pr-test-") as temporary:
            root = pathlib.Path(temporary)
            repository = root / "checkout"
            repository.mkdir()
            git(repository, "init", "--initial-branch=main")
            git(repository, "config", "user.name", "Release test")
            git(repository, "config", "user.email", "test@example.invalid")
            (repository / "packages/demo").mkdir(parents=True)
            (repository / "catalog").mkdir()
            (repository / "Cargo.toml").write_text('[workspace.package]\nversion = "1.0.0"\n')
            (repository / "Cargo.lock").write_text("# fixture\n")
            (repository / "catalog/packages.json").write_text("[]\n")
            (repository / "packages/demo/Cargo.toml").write_text('[package]\nname = "demo"\n')
            git(repository, "add", ".")
            git(repository, "commit", "-m", "initial main")
            remote = root / "remote.git"
            subprocess.run(["git", "clone", "--bare", str(repository), str(remote)], check=True, capture_output=True)
            git(repository, "remote", "add", "origin", str(remote))
            original_main = git(repository, "rev-parse", "HEAD")
            (repository / "Cargo.toml").write_text('[workspace.package]\nversion = "1.0.1"\n')

            binary = root / "bin"
            binary.mkdir()
            gh = binary / "gh"
            gh.write_text("""#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  'pr create') echo 'https://github.com/example/repo/pull/1' ;;
  'pr merge')
    tree="$(git rev-parse 'HEAD^{tree}')"
    parent="$(git ls-remote origin refs/heads/main | cut -f1)"
    merged="$(printf 'squashed version PR\\n' | git commit-tree "$tree" -p "$parent")"
    git push origin "$merged:refs/heads/main" >&2
    printf '%s\\n' "$merged" > "$FAKE_MERGED"
    ;;
  'pr view') cat "$FAKE_MERGED" ;;
  *) exit 99 ;;
esac
""", encoding="utf-8", newline="\n")
            gh.chmod(0o755)
            output = root / "outputs.txt"
            script = pathlib.Path(__file__).with_name("release-version-pr.sh")
            environment = dict(os.environ, GITHUB_RUN_ID="123", GITHUB_RUN_ATTEMPT="1",
                               GITHUB_OUTPUT=shell_path(output), RUNNER_TEMP=shell_path(root),
                               FAKE_MERGED=shell_path(root / "merged.txt"),
                               FIXTURE_BIN=shell_path(binary), RELEASE_SCRIPT=shell_path(script))
            result = subprocess.run(["bash", "-c", 'export PATH="$FIXTURE_BIN:$PATH"; bash "$RELEASE_SCRIPT"'],
                                    cwd=repository, env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            merged = git(repository, "rev-parse", "HEAD")
            self.assertNotEqual(merged, original_main)
            self.assertEqual(output.read_text().strip(), f"source_ref={merged}")
            self.assertEqual(git(remote, "rev-parse", "refs/heads/main"), merged)
            self.assertEqual(git(repository, "status", "--porcelain"), "")
            self.assertEqual(git(remote, "branch", "--list", "codex/release-*"), "")


if __name__ == "__main__":
    unittest.main()
