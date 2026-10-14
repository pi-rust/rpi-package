"""Include completed and retryable package releases pointing at this checkout."""

import json
import os
import subprocess


def output(*command):
    return subprocess.check_output(command, encoding="utf-8").strip()


def select(metadata, refs, revision):
    tags = []
    for package in metadata["packages"]:
        if package["id"] not in metadata["workspace_members"] or package["publish"] == []:
            continue
        tag = f"{package['name']}-v{package['version']}"
        ref = f"refs/tags/{tag}"
        if refs.get(ref + "^{}", refs.get(ref)) == revision:
            tags.append(tag)
    return sorted(tags)


def main():
    revision = output("git", "rev-parse", "HEAD")
    refs = {}
    for line in output("git", "ls-remote", "--tags", "origin").splitlines():
        sha, ref = line.split()
        refs[ref] = sha
    metadata = json.loads(output("cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"))
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as destination:
        destination.write(f"package_releases={json.dumps(select(metadata, refs, revision), separators=(',', ':'))}\n")
        destination.write(f"bundle_tag=rpi-packages-{revision[:12]}\n")


if __name__ == "__main__":
    main()
