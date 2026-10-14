"""Synchronize the human catalog with the actual publishable Cargo packages."""

import json
import pathlib
import subprocess

metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"
]))
packages = {package["name"]: package for package in metadata["packages"]}
path = pathlib.Path("catalog/packages.json")
catalog = json.loads(path.read_text(encoding="utf-8"))
for entry in catalog:
    package = packages[entry["name"]]
    entry["version"] = package["version"]
    entry["repository"] = package["repository"]
    entry["prebuilt_manifest"] = (
        f"{package['repository'].removesuffix('.git').rstrip('/')}/releases/download/"
        f"{package['name']}-v{package['version']}/rpi-extension.json"
    )
path.write_text(json.dumps(catalog, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
