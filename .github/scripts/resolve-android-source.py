#!/usr/bin/env python3
"""Select a reviewed Android commit from a workflow dispatched on main, before keys exist."""
import argparse
import os
from pathlib import Path
import re
import subprocess


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, stdout=subprocess.PIPE, encoding="utf-8").stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", default="")
    args = parser.parse_args()
    if os.environ.get("GITHUB_EVENT_NAME") != "workflow_dispatch" or os.environ.get("GITHUB_REF") != "refs/heads/main":
        raise SystemExit("Signed Android builds must be dispatched from main")
    if args.tag and not re.fullmatch(r"rpi-web-android-v\d+\.\d+\.\d+", args.tag):
        raise SystemExit("Invalid Android release tag")
    git("fetch", "--no-tags", "origin", "main:refs/remotes/origin/main")
    if args.tag:
        git("fetch", "--force", "origin", f"refs/tags/{args.tag}:refs/tags/{args.tag}")
        source = f"refs/tags/{args.tag}^{{commit}}"
    else:
        source = os.environ["GITHUB_SHA"]
        if not re.fullmatch(r"[a-f0-9]{40,64}", source):
            raise SystemExit("Invalid workflow source commit")
    commit = git("rev-parse", "--verify", source)
    git("merge-base", "--is-ancestor", commit, "origin/main")
    if args.tag:
        properties = git("show", f"{commit}:packages/rpi-web/android/version.properties")
        match = re.search(r"^versionName=(\d+\.\d+\.\d+)$", properties, re.M)
        if not match or args.tag != f"rpi-web-android-v{match[1]}":
            raise SystemExit("Tag does not match Android version.properties")
    if git("status", "--porcelain"):
        raise SystemExit("Source selection requires a clean checkout")
    git("checkout", "--detach", commit)
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write(f"source_commit={commit}\n")
    print("Validated Android source commit:", commit)


if __name__ == "__main__":
    main()
