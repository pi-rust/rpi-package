#!/usr/bin/env python3
"""Render `registry/rpi-acp/agent.json` from a published bundle release.

The ACP Registry wants one `distribution.binary` entry per platform, each with an
archive URL that pins the version and a matching `sha256`. Every one of those
values already exists in the extension-bundle release's `rpi-packages.json`
(`targets.<triple>.acp_bridge`), so this reads that manifest instead of asking a
human to copy five checksums by hand.

    # after the release workflow has built the bundle
    python3 registry/render-agent-json.py --bundle-tag rpi-packages-<sha12>
    python3 registry/render-agent-json.py --bundle-tag rpi-packages-<sha12> --check

`--check` exits non-zero when the committed file differs from what the release
would render, which is what a CI gate on "the registry entry matches the last
release" would run.
"""
import argparse
import json
import pathlib
import re
import sys
import urllib.error
import urllib.request

# Cargo target triple -> the registry's GOOS-GOARCH platform key (FORMAT.md).
PLATFORMS = {
    "aarch64-apple-darwin": "darwin-aarch64",
    "x86_64-apple-darwin": "darwin-x86_64",
    "aarch64-unknown-linux-gnu": "linux-aarch64",
    "x86_64-unknown-linux-gnu": "linux-x86_64",
    "aarch64-pc-windows-msvc": "windows-aarch64",
    "x86_64-pc-windows-msvc": "windows-x86_64",
}

REPOSITORY = "https://github.com/pi-rust/rpi-package"
AGENT_ID = "rpi-acp"
ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "registry" / "rpi-acp" / "agent.json"


def package_version():
    """`version = "x.y.z"` from packages/rpi-acp/Cargo.toml."""
    text = (ROOT / "packages" / "rpi-acp" / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not match:
        raise SystemExit("cannot read rpi-acp's version from packages/rpi-acp/Cargo.toml")
    return match.group(1)


def load_bundle_manifest(bundle_tag, manifest_url=None):
    url = manifest_url or (
        f"{REPOSITORY}/releases/download/{bundle_tag}/rpi-packages.json"
    )
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            return json.load(response)
    except urllib.error.URLError as error:
        raise SystemExit(f"cannot read {url}: {error}") from error


def render(version, bundle_tag, manifest):
    """Build the registry entry, or fail when a platform is missing."""
    targets = manifest["targets"]
    binary = {}
    for triple, platform in PLATFORMS.items():
        fragment = targets.get(triple)
        if not fragment:
            # Only the platforms the release actually built are listed.
            print(f"note: no build for {triple}, skipping {platform}", file=sys.stderr)
            continue
        bridge = fragment["acp_bridge"]
        binary[platform] = {
            "archive": bridge["url"],
            "cmd": "./rpi-acp-bridge.exe" if "windows" in triple else "./rpi-acp-bridge",
            "args": [],
            "sha256": bridge["sha256"],
        }
    if not binary:
        raise SystemExit("the bundle manifest lists no ACP bridge artifacts")
    entry = {
        "id": AGENT_ID,
        "name": "rpi",
        "version": version,
        "description": "Rust coding agent (rpi) exposed over the Agent Client Protocol",
        "repository": REPOSITORY,
        "website": f"{REPOSITORY}/tree/main/packages/rpi-acp",
        "authors": ["BigFish"],
        "license": "MIT",
        "license_url": f"{REPOSITORY}/blob/main/LICENSE",
        "distribution": {"binary": binary},
    }
    return entry


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bundle-tag",
        required=True,
        help="Extension bundle release tag, e.g. rpi-packages-7249c71c5a9d",
    )
    parser.add_argument("--manifest-url", help="Override the rpi-packages.json URL")
    parser.add_argument(
        "--check",
        action="store_true",
        help="Compare with the committed agent.json instead of writing it",
    )
    args = parser.parse_args()

    version = package_version()
    manifest = load_bundle_manifest(args.bundle_tag, args.manifest_url)
    entry = render(version, args.bundle_tag, manifest)
    rendered = json.dumps(entry, indent=2, ensure_ascii=False) + "\n"

    # The registry matches the archive URL's version against `version`, so a
    # bundle built from a different source tree would be rejected there; catch it
    # here where the message is clearer.
    for platform, target in entry["distribution"]["binary"].items():
        if version not in target["archive"]:
            print(
                f"warning: {platform} archive URL does not pin {version}: "
                f"{target['archive']}",
                file=sys.stderr,
            )

    if args.check:
        committed = MANIFEST.read_text(encoding="utf-8") if MANIFEST.exists() else ""
        if committed != rendered:
            print(f"{MANIFEST} does not match {args.bundle_tag}", file=sys.stderr)
            print(rendered, file=sys.stderr)
            return 1
        print(f"{MANIFEST} matches {args.bundle_tag} ({version})")
        return 0

    MANIFEST.write_text(rendered, encoding="utf-8")
    print(f"wrote {MANIFEST} ({version}, {len(entry['distribution']['binary'])} platform(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
