"""Select workspace package identities even when registry dependencies share names."""
import json
from pathlib import Path
import subprocess
import sys


def main():
    root = Path(__file__).resolve().parents[2]
    arguments = sys.argv[1:]
    config = []
    for index, value in enumerate(arguments):
        if value == "--":
            break
        if value == "--config" and index + 1 < len(arguments):
            config.extend([value, arguments[index + 1]])
    # Packaging against an unpublished workspace patch intentionally changes
    # source identities. Resolve that graph offline first, verify it locked,
    # and restore the checked-in graph even if verification fails.
    lock = root / "Cargo.lock"
    original = lock.read_bytes()
    try:
        return run(root, arguments, config)
    finally:
        if config:
            lock.write_bytes(original)


def run(root, arguments, config):
    if config:
        subprocess.run(["cargo", "update", "--workspace", "--offline", *config], cwd=root, check=True)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1", *config],
        cwd=root, encoding="utf-8",
    ))
    members = set(metadata["workspace_members"])
    identities = {p["name"]: p["id"] for p in metadata["packages"] if p["id"] in members}
    for index, value in enumerate(arguments):
        if value == "--":
            break
        if value in ("-p", "--package") and index + 1 < len(arguments):
            arguments[index + 1] = identities.get(arguments[index + 1], arguments[index + 1])
    return subprocess.call(["cargo", *arguments], cwd=root)


if __name__ == "__main__":
    raise SystemExit(main())
