"""Include completed and retryable package releases pointing at this checkout."""

import json
import os
import subprocess


def output(*command):
    return subprocess.check_output(command, encoding="utf-8").strip()


def select(metadata, refs, revision):
    tags = []
    for package in metadata["packages"]:
        # Web publishes its embedded-frontend extension beside the standalone
        # archive in release-web.yml, using one verified frontend build.
        if package["name"] == "rpi-web":
            continue
        if package["id"] not in metadata["workspace_members"] or package["publish"] == []:
            continue
        if not any("cdylib" in target["crate_types"] for target in package["targets"]):
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
        destination.write(f"web_tag={select_web(metadata, refs, revision)}\n")


def select_web(metadata, refs, revision):
    for package in metadata["packages"]:
        if (package["id"] in metadata["workspace_members"] and package["name"] == "rpi-web"
                and package["publish"] != []
                and any("bin" in target["crate_types"] for target in package["targets"])):
            tag = f"rpi-web-v{package['version']}"
            ref = f"refs/tags/{tag}"
            if refs.get(ref + "^{}", refs.get(ref)) == revision:
                return tag
    return ""


if __name__ == "__main__":
    main()
