"""Keep release-plz's temporary historical checkouts independent of pi-rust."""

import functools
import pathlib
import subprocess
import tomllib


def git(*args):
    return subprocess.check_output(["git", *args], encoding="utf-8").strip()


manifests = ["Cargo.toml", *sorted(
    path.as_posix() for path in pathlib.Path("packages").glob("*/Cargo.toml")
)]


@functools.lru_cache(maxsize=None)
def requires_external_sdk(revision):
    for manifest in manifests:
        result = subprocess.run(
            ["git", "show", f"{revision}:{manifest}"],
            capture_output=True, encoding="utf-8",
        )
        # Newly added packages do not exist in older commits.
        if result.returncode:
            continue
        document = tomllib.loads(result.stdout)
        for dependencies in (
            document.get("dependencies", {}),
            document.get("workspace", {}).get("dependencies", {}),
        ):
            sdk = dependencies.get("rpi-plugin-sdk")
            if isinstance(sdk, dict) and "path" in sdk:
                return True
    return False


head = git("rev-parse", "HEAD")
if requires_external_sdk(head):
    raise SystemExit("The release checkout must use the published rpi-plugin-sdk.")

pending = [head]
visited = set()
boundaries = set()
while pending:
    revision = pending.pop()
    if revision in visited:
        continue
    visited.add(revision)
    parents = git("rev-list", "--parents", "-n", "1", revision).split()[1:]
    if any(requires_external_sdk(parent) for parent in parents):
        boundaries.add(revision)
    else:
        pending.extend(parents)

if boundaries:
    # Only this CI checkout is shallow. Remote history and tags stay intact.
    shallow = pathlib.Path(git("rev-parse", "--git-path", "shallow"))
    existing = shallow.read_text().splitlines() if shallow.exists() else []
    shallow.write_text("\n".join(sorted(boundaries | set(existing))) + "\n")
    print(f"Release analysis excludes external SDK history at {len(boundaries)} boundary commits.")
