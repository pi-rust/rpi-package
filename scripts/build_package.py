#!/usr/bin/env python3
"""Build an rpi Agent package and its release manifest."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from io import BytesIO
import json
from pathlib import Path
import re
import tarfile
from urllib.request import Request, urlopen
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

ROOT = Path(__file__).resolve().parents[1]
PACKAGE_NAME = re.compile(r"[a-z][a-z0-9_-]{0,63}")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}(?:-[0-9A-Za-z.-]+)?")
LIBRARIES = {"aarch64-apple-darwin": ".dylib", "x86_64-pc-windows-msvc": ".dll"}
MAX_FILE = 64 * 1024 * 1024


def checked_download(url, size, digest):
    if not 0 < size <= MAX_FILE:
        raise ValueError(f"extension archive size is invalid: {url}")
    with urlopen(Request(url, headers={"User-Agent": "rpi-package-builder"}), timeout=45) as response:
        data = response.read(size + 1)
    if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
        raise ValueError(f"extension archive checksum mismatch: {url}")
    return data


def fetch_extension(extension, target):
    item = extension["targets"][target]
    name, version, repository = (extension[key] for key in ("name", "version", "repository"))
    kind = item["format"]
    if kind not in {"zip", "tar.gz"}:
        raise ValueError(f"unsupported release archive format: {kind}")
    expected = f"https://github.com/{repository}/releases/download/{name}-v{version}/{name}-{version}-{target}.{kind}"
    path = item["library"]["path"]
    if item["url"] != expected or not re.fullmatch(r"[A-Za-z0-9_]+" + re.escape(LIBRARIES[target]), path):
        raise ValueError(f"invalid extension origin or library path: {name} {target}")
    data = checked_download(expected, item["size"], item["sha256"])
    if kind == "zip":
        with ZipFile(BytesIO(data)) as archive:
            entry = archive.getinfo(path)
            if entry.is_dir() or entry.file_size > MAX_FILE:
                raise ValueError(f"invalid extension member: {path}")
            library = archive.read(entry)
    else:
        with tarfile.open(fileobj=BytesIO(data), mode="r:gz") as archive:
            entry = archive.getmember(path)
            if not entry.isfile() or entry.size > MAX_FILE:
                raise ValueError(f"invalid extension member: {path}")
            with archive.extractfile(entry) as source:
                library = source.read()
    if hashlib.sha256(library).hexdigest() != item["library"]["sha256"]:
        raise ValueError(f"extension library checksum mismatch: {path}")
    return f"extensions/{path}", library


def package_files(package, manifest):
    files = {"rpi-package.json": package / "rpi-package.json"}
    for dirname in ("instructions", "skills", "optional-skills", "commands"):
        for path in sorted((package / dirname).rglob("*")):
            if path.is_symlink():
                raise ValueError(f"symlink in package: {path}")
            if path.is_file():
                files[path.relative_to(package).as_posix()] = path
    required = manifest["agent"]["instructions"] + [command["template"] for command in manifest["commands"].values()]
    for path in required:
        if path not in files:
            raise ValueError(f"missing packaged resource: {path}")
    if any(path.resolve().relative_to(package.resolve()).as_posix() != name for name, path in files.items()):
        raise ValueError("package contains aliased paths")
    return files


def write_archive(archive, files, binaries):
    if len(set(files) | {name for name, _ in binaries}) != len(files) + len(binaries):
        raise ValueError("duplicate package archive path")
    with ZipFile(archive, "w", compression=ZIP_DEFLATED) as zip_file:
        for name, source in sorted(files.items()):
            write_entry(zip_file, name, source.read_bytes(), source.stat().st_mode & 0o755)
        for name, data in sorted(binaries):
            write_entry(zip_file, name, data, 0o644)
    data = archive.read_bytes()
    return {"asset": archive.name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def write_entry(archive, name, data, permissions):
    if len(data) > MAX_FILE:
        raise ValueError(f"oversized package resource: {name}")
    entry = ZipInfo(name)
    entry.create_system = 3
    entry.external_attr = (0o100000 | permissions) << 16
    archive.writestr(entry, data, compress_type=ZIP_DEFLATED)


def build(name, output, targets=None, tag=None):
    if not PACKAGE_NAME.fullmatch(name):
        raise ValueError("invalid package name")
    package = ROOT / "packages" / name
    manifest = json.loads((package / "rpi-package.json").read_text(encoding="utf-8"))
    version = manifest["version"]
    if manifest["schema_version"] != 1 or manifest["name"] != name or not VERSION.fullmatch(version):
        raise ValueError("package manifest identity or version is invalid")
    if tag is not None and tag != f"{name}-v{version}":
        raise ValueError("release tag does not match package name and version")

    files = package_files(package, manifest)
    lock_path = package / "extensions.lock.json"
    release = {"schema_version": 1, "name": name, "version": version}
    output.mkdir(parents=True, exist_ok=True)
    if lock_path.exists():
        lock = json.loads(lock_path.read_text(encoding="utf-8"))
        files[lock_path.name] = lock_path
        chosen = targets or lock["targets"]
        if any(target not in lock["targets"] or target not in LIBRARIES for target in chosen):
            raise ValueError("package does not support the requested target")
        release["targets"] = {}
        with ThreadPoolExecutor(max_workers=4) as workers:
            for target in dict.fromkeys(chosen):
                binaries = list(workers.map(lambda extension: fetch_extension(extension, target), lock["extensions"]))
                release["targets"][target] = write_archive(output / f"{name}-{version}-{target}.zip", files, binaries)
    elif targets:
        raise ValueError("resource-only packages do not require a target")
    else:
        release.update(write_archive(output / f"{name}-{version}.zip", files, []))
    (output / "release.json").write_text(json.dumps(release, indent=2) + "\n", encoding="utf-8")
    return release


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("name")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--target", action="append", help="build only the specified target; repeat for multiple targets")
    parser.add_argument("--tag", help="require the tag to match this package release")
    options = parser.parse_args()
    build(options.name, options.out, options.target, options.tag)
    print(options.out / "release.json")


if __name__ == "__main__":
    main()
