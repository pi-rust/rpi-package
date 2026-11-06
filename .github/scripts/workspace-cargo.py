"""Select workspace package identities even when registry dependencies share names."""
import json
from pathlib import Path
import subprocess
import sys


def main():
    root = Path(__file__).resolve().parents[2]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=root, encoding="utf-8",
    ))
    members = set(metadata["workspace_members"])
    identities = {p["name"]: p["id"] for p in metadata["packages"] if p["id"] in members}
    arguments = sys.argv[1:]
    for index, value in enumerate(arguments):
        if value == "--":
            break
        if value in ("-p", "--package") and index + 1 < len(arguments):
            arguments[index + 1] = identities.get(arguments[index + 1], arguments[index + 1])
    return subprocess.call(["cargo", *arguments], cwd=root)


if __name__ == "__main__":
    raise SystemExit(main())
