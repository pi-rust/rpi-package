"""Publish portable rpi-web binaries with an identical verified frontend."""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile

spec = importlib.util.spec_from_file_location("extension_assets", Path(__file__).with_name("prebuilt-assets.py"))
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


def frontend_inventory(root):
    html = (root / "index.html").read_text(encoding="utf-8")
    references = re.findall(r'(?:src|href)="(/assets/[^\"]+)"', html)
    if not references or any(not (root / name.lstrip("/")).is_file() for name in references):
        raise ValueError("Frontend index references missing assets")
    files = sorted(path for path in root.rglob("*") if path.is_file())
    if any(path.is_symlink() for path in files):
        raise ValueError("Frontend must contain regular files")
    return {path.relative_to(root).as_posix(): assets.digest(path) for path in files}


def build(args):
    metadata = json.loads(assets.command("cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"))
    package = next(package for package in metadata["packages"] if package["name"] == "rpi-web")
    if args.tag != f"rpi-web-v{package['version']}":
        raise ValueError("Tag does not match the Web package version")
    if assets.command("git", "rev-parse", "HEAD") != args.source_ref:
        raise ValueError("Checkout differs from the release commit")
    host = re.search(r"^host: (.+)$", assets.command("rustc", "-vV"), re.MULTILINE).group(1)
    if args.target not in assets.TARGETS or args.target != host:
        raise ValueError("The build target must match the native compiler")
    windows = "windows" in args.target
    binary_name = "rpi-web.exe" if windows else "rpi-web"
    binary = Path(metadata["target_directory"]) / "release" / binary_name
    if assets.command(str(binary), "--version") != f"rpi-web {package['version']}":
        raise ValueError("Binary reports the wrong package version")
    root = Path(args.assets)
    inventory = frontend_inventory(root)
    # The generic extension archive uses rpi-web-VERSION-TARGET; keep the
    # standalone archive distinct when both are attached to the same release.
    prefix = f"rpi-web-standalone-{package['version']}-{args.target}"
    archive = Path(args.output) / (prefix + (".zip" if windows else ".tar.gz"))
    archive.parent.mkdir(parents=True, exist_ok=True)
    files = [(binary, f"{prefix}/{binary_name}"),
             (Path(package["manifest_path"]).parent / "README.md", f"{prefix}/README.md")]
    files += [(root / name, f"{prefix}/assets/{name}") for name in inventory]
    if windows:
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            for path, name in files:
                bundle.write(path, name)
    else:
        with tarfile.open(archive, "w:gz") as bundle:
            for path, name in files:
                bundle.add(path, arcname=name, recursive=False)
    distribution = assets.asset(archive, args.repository, args.tag)
    distribution["binary"] = {"path": f"{prefix}/{binary_name}", "sha256": assets.digest(binary)}
    assets.write_json(Path(args.output) / f"rpi-web.{args.target}.json", {
        "schema_version": 1, "name": "rpi-web", "version": package["version"],
        "tag": args.tag, "repository": f"https://github.com/{args.repository}",
        "source_commit": args.source_ref, "target": args.target,
        "frontend": inventory, "distribution": distribution,
    })


def publish(args):
    root = Path(args.output)
    fragments = assets.collect(root, "rpi-web.*.json", args.source_ref)
    first = fragments[0]
    if not re.fullmatch(r"rpi-web-v\d+\.\d+\.\d+", args.tag):
        raise ValueError("Invalid Web release tag")
    for fragment in fragments:
        if (fragment["schema_version"] != 1 or fragment["name"] != "rpi-web"
                or fragment["tag"] != args.tag or f"rpi-web-v{fragment['version']}" != args.tag
                or fragment["repository"] != f"https://github.com/{args.repository}"
                or fragment["frontend"] != first["frontend"]):
            raise ValueError("Platform metadata or frontend contents differ")
    archives = [assets.checked_archive(root, fragment["distribution"]) for fragment in fragments]
    assets.check_tag_source(args.repository, args.tag, args.source_ref)
    existing = json.loads(assets.command("gh", "release", "view", args.tag, "--repo", args.repository, "--json", "assets"))
    if any(asset["name"] == "rpi-web-release.json" for asset in existing["assets"]):
        print("Preserving the completed immutable Web release")
        return
    manifest = {key: first[key] for key in ("schema_version", "name", "version", "tag", "repository", "source_commit", "frontend")}
    manifest["targets"] = {fragment["target"]: fragment["distribution"] for fragment in fragments}
    path = root / "rpi-web-release.json"
    assets.write_json(path, manifest)
    subprocess.run(["gh", "release", "upload", args.tag, "--repo", args.repository,
                    *map(str, archives), "--clobber"], check=True)
    subprocess.run(["gh", "release", "upload", args.tag, "--repo", args.repository,
                    str(path)], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "publish"))
    parser.add_argument("--source-ref", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--target")
    parser.add_argument("--assets", default="packages/rpi-web/assets")
    parser.add_argument("--output", default="dist/web-release")
    args = parser.parse_args()
    (build if args.command == "build" else publish)(args)
