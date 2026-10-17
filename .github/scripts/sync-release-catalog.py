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
    # Source-only extensions do not advertise releases unless they explicitly
    # opt into GitHub prebuilt distribution independently of crates.io.
    prebuilt = package.get("metadata", {}).get("rpi", {}).get("prebuilt", False)
    if package.get("publish") == [] and not prebuilt:
        entry.pop("prebuilt_manifest", None)
        continue
    entry["prebuilt_manifest"] = (
        f"{package['repository'].removesuffix('.git').rstrip('/')}/releases/download/"
        f"{package['name']}-v{package['version']}/rpi-extension.json"
    )
path.write_text(json.dumps(catalog, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
