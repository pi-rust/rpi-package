"""Build and publish versioned extension assets using the v1 distribution protocol."""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import tarfile
import zipfile

TARGETS = {
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}


def command(*args):
    return subprocess.check_output(args, encoding="utf-8").strip()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def asset(path, repository, tag):
    return {
        "url": f"https://github.com/{repository}/releases/download/{tag}/{path.name}",
        "sha256": digest(path),
        "size": path.stat().st_size,
        "format": "zip" if path.suffix == ".zip" else "tar.gz",
    }


def archive_library(library, destination):
    if destination.suffix == ".zip":
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.write(library, library.name)
    else:
        with tarfile.open(destination, "w:gz") as archive:
            archive.add(library, arcname=library.name)


def sdk_identity(package, metadata):
    by_id = {item["id"]: item for item in metadata["packages"]}
    node = next(item for item in metadata["resolve"]["nodes"] if item["id"] == package["id"])
    sdk_id = next(dep["pkg"] for dep in node["deps"] if dep["name"] == "rpi_plugin_sdk")
    sdk = by_id[sdk_id]
    source = pathlib.Path(sdk["manifest_path"]).parent / "src/lib.rs"
    match = re.search(r"pub const RPI_PLUGIN_ABI_VERSION_UNIFIED:\s*u32\s*=\s*(\d+)", source.read_text(encoding="utf-8"))
    if not match:
        raise ValueError(f"Cannot determine the plugin ABI of SDK {sdk['version']}")
    return {"version": sdk["version"], "plugin_abi": int(match.group(1))}


def build(args):
    metadata = json.loads(command("cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", args.target))
    host = re.search(r"^host: (.+)$", command("rustc", "-vV"), re.MULTILINE).group(1)
    if host != args.target:
        raise ValueError(f"Native build target {host} does not match {args.target}")
    revision = command("git", "rev-parse", "HEAD")
    if revision != args.source_ref:
        raise ValueError("The build checkout differs from the release commit")
    windows = "windows" in args.target
    macos = "apple-darwin" in args.target
    extension = "dll" if windows else "dylib" if macos else "so"
    archive_extension = "zip" if windows else "tar.gz"
    release_dir = pathlib.Path(metadata["target_directory"]) / "release"
    output = pathlib.Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    selected = set(json.loads(args.tags))
    found = set()
    packages = []
    for package in metadata["packages"]:
        if package["id"] not in metadata["workspace_members"]:
            continue
        library_target = next((target for target in package["targets"] if "cdylib" in target["crate_types"]), None)
        if not library_target:
            continue
        tag = f"{package['name']}-v{package['version']}"
        if getattr(args, "packages_only", False) and tag not in selected:
            continue
        stem = library_target["name"] if windows else "lib" + library_target["name"]
        library = release_dir / f"{stem}.{extension}"
        if not library.is_file():
            raise ValueError(f"Missing compiled library: {library}")
        tag = f"{package['name']}-v{package['version']}"
        record = {
            "name": package["name"], "version": package["version"],
            "library": {"path": library.name, "sha256": digest(library)},
            "sdk": sdk_identity(package, metadata),
        }
        packages.append(record)
        if tag not in selected:
            continue
        expected_repository = f"https://github.com/{args.repository}"
        if package["repository"].rstrip("/").removesuffix(".git") != expected_repository:
            raise ValueError(f"{package['name']} repository metadata does not match the publishing repository")
        found.add(tag)
        archive = output / f"{package['name']}-{package['version']}-{args.target}.{archive_extension}"
        archive_library(library, archive)
        distribution = asset(archive, args.repository, tag)
        distribution["library"] = record["library"]
        fragment = {
            "schema_version": 1, "name": package["name"], "version": package["version"],
            "repository": expected_repository, "tag": tag, "source_commit": revision,
            "sdk": record["sdk"], "target": args.target, "distribution": distribution,
        }
        write_json(output / f"{package['name']}.{args.target}.json", fragment)
    if selected != found:
        raise ValueError(f"Release tags do not match workspace versions: {sorted(selected - found)}")
    if getattr(args, "packages_only", False):
        if not selected:
            raise ValueError("Package-only builds require at least one release tag")
        return

    os_name = "windows" if windows else "macos" if macos else "linux"
    arch = args.target.split("-")[0]
    platform = f"{os_name}-{arch}"
    bundle = pathlib.Path("dist") / f"rpi-packages-{platform}.{archive_extension}"
    bridge = pathlib.Path("dist") / f"rpi-acp-{platform}.{archive_extension}"
    # The installer has already produced the bundle and standalone ACP bridge.
    import shutil
    for source in (bundle, bridge):
        shutil.copy2(source, output / source.name)
    write_json(output / f"bundle.{args.target}.json", {
        "schema_version": 1, "repository": f"https://github.com/{args.repository}",
        "tag": args.bundle_tag, "source_commit": revision, "target": args.target,
        "archive": asset(output / bundle.name, args.repository, args.bundle_tag),
        "acp_bridge": asset(output / bridge.name, args.repository, args.bundle_tag),
        "packages": packages,
    })


def release_exists(tag):
    return subprocess.run(["gh", "release", "view", tag], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def check_tag_source(repository, tag, revision):
    from urllib.parse import quote
    target = json.loads(command("gh", "api", f"repos/{repository}/git/ref/tags/{quote(tag, safe='')}"))["object"]
    while target["type"] == "tag":
        target = json.loads(command("gh", "api", f"repos/{repository}/git/tags/{target['sha']}"))["object"]
    if target["type"] != "commit" or target["sha"] != revision:
        raise ValueError(f"Release tag {tag} does not point to the build commit")


def upload(tag, paths):
    subprocess.run(["gh", "release", "upload", tag, *map(str, paths), "--clobber"], check=True)


def collect(root, pattern, source_ref):
    fragments = [json.loads(path.read_text(encoding="utf-8")) for path in root.glob(pattern)]
    targets = [fragment["target"] for fragment in fragments]
    if set(targets) != TARGETS or len(targets) != len(TARGETS):
        raise ValueError(f"Missing or duplicate platform artifacts for {pattern}: {targets}")
    if any(fragment["source_commit"] != source_ref for fragment in fragments):
        raise ValueError("Platform artifacts were built from different source commits")
    return fragments


def checked_archive(root, distribution):
    name = distribution["url"].rsplit("/", 1)[1]
    path = root / name
    if digest(path) != distribution["sha256"] or path.stat().st_size != distribution["size"]:
        raise ValueError(f"Archive checksum or size does not match: {name}")
    return path


def publish(args):
    root = pathlib.Path(args.output)
    # Validate all platforms and prepare all manifests before any upload.
    prepared = []
    for tag in json.loads(args.tags):
        name, version = tag.rsplit("-v", 1)
        fragments = collect(root, f"{name}.*.json", args.source_ref)
        first = fragments[0]
        identity = (name, version, first["repository"], tag, first["sdk"])
        for fragment in fragments:
            if (fragment["name"], fragment["version"], fragment["repository"], fragment["tag"], fragment["sdk"]) != identity:
                raise ValueError(f"Platform package identity differs for {tag}")
        manifest = {key: first[key] for key in ("schema_version", "name", "version", "repository", "tag", "source_commit", "sdk")}
        manifest["targets"] = {fragment["target"]: fragment["distribution"] for fragment in fragments}
        path = root / "manifests" / tag / "rpi-extension.json"
        write_json(path, manifest)
        paths = [checked_archive(root, fragment["distribution"]) for fragment in fragments]
        prepared.append((tag, path, paths))

    if getattr(args, "packages_only", False):
        if not prepared:
            raise ValueError("Package-only publishing requires at least one release tag")
        for tag, _, _ in prepared:
            check_tag_source(args.repository, tag, args.source_ref)
            if not release_exists(tag):
                raise ValueError(f"Package Release must exist before uploading assets: {tag}")
        for tag, manifest, paths in prepared:
            existing = json.loads(command("gh", "release", "view", tag, "--json", "assets"))["assets"]
            if any(item["name"] == "rpi-extension.json" for item in existing):
                print(f"Preserving already published immutable assets: {tag}")
                continue
            upload(tag, paths)
            upload(tag, [manifest])
        return

    bundles = collect(root, "bundle.*.json", args.source_ref)
    bundle_manifest = {
        "schema_version": 1, "repository": f"https://github.com/{args.repository}",
        "tag": args.bundle_tag, "source_commit": args.source_ref,
        "targets": {fragment["target"]: {key: fragment[key] for key in ("archive", "acp_bridge", "packages")} for fragment in bundles},
    }
    bundle_paths = []
    for fragment in bundles:
        if fragment["tag"] != args.bundle_tag:
            raise ValueError("Bundle tags differ between platforms")
        bundle_paths.extend(checked_archive(root, fragment[key]) for key in ("archive", "acp_bridge"))
    bundle_manifest_path = root / "rpi-packages.json"
    write_json(bundle_manifest_path, bundle_manifest)

    for tag, _, _ in prepared:
        check_tag_source(args.repository, tag, args.source_ref)
    bundle_exists = release_exists(args.bundle_tag)
    if bundle_exists:
        check_tag_source(args.repository, args.bundle_tag, args.source_ref)

    for tag, manifest, paths in prepared:
        if not release_exists(tag):
            raise ValueError(f"Package Release must exist before uploading assets: {tag}")
        existing_assets = json.loads(command("gh", "release", "view", tag, "--json", "assets"))["assets"]
        if any(item["name"] == "rpi-extension.json" for item in existing_assets):
            print(f"Preserving already published immutable assets: {tag}")
            continue
        upload(tag, paths)
        # Publish the discovery manifest last, after all five archives exist.
        upload(tag, [manifest])

    if not bundle_exists:
        subprocess.run(["gh", "release", "create", args.bundle_tag, "--target", args.source_ref,
                        "--title", f"Extension bundle {args.source_ref[:12]}", "--generate-notes", "--latest=false"], check=True)
    bundle_assets = json.loads(command("gh", "release", "view", args.bundle_tag, "--json", "assets"))["assets"]
    if not any(item["name"] == "rpi-packages.json" for item in bundle_assets):
        upload(args.bundle_tag, bundle_paths)
        upload(args.bundle_tag, [bundle_manifest_path])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "publish"))
    parser.add_argument("--target", choices=sorted(TARGETS))
    parser.add_argument("--repository", required=True)
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--bundle-tag", required=True)
    parser.add_argument("--tags", default="[]", help="JSON array of per-package release tags")
    parser.add_argument("--packages-only", action="store_true", help="Build/publish selected packages without the full workspace bundle")
    parser.add_argument("--output", default="dist/release-assets")
    args = parser.parse_args()
    if args.command == "build" and not args.target:
        parser.error("build requires --target")
    (build if args.command == "build" else publish)(args)


if __name__ == "__main__":
    main()
