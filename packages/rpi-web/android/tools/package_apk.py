#!/usr/bin/env python3
"""Verify and package Android outputs; never sign, read passwords, or infer release from a filename."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
APPLICATION_ID = "cn.bigfisharc.rpiweb.local"


def version(root: Path = ROOT) -> tuple[str, int]:
    values = dict(line.split("=", 1) for line in (root / "version.properties").read_text().splitlines() if "=" in line)
    name, code = values["versionName"], int(values["versionCode"])
    if not re.fullmatch(r"\d+\.\d+\.\d+", name) or not 1 <= code <= 2100000000:
        raise ValueError("Invalid Android version.properties")
    return name, code


def check_tag(tag: str, name: str) -> None:
    if tag != f"rpi-web-android-v{name}":
        raise ValueError("Android tag must exactly match version.properties")


def check_badging(text: str, variant: str, name: str, code: int) -> None:
    match = re.search(r"^package: name='([^']+)' versionCode='(\d+)' versionName='([^']+)'", text, re.M)
    if not match or match.groups() != (APPLICATION_ID, str(code), name):
        raise ValueError("APK application identity or version mismatch")
    if not re.search(r"^sdkVersion:'26'$", text, re.M) or not re.search(r"^targetSdkVersion:'35'$", text, re.M):
        raise ValueError("Unexpected Android SDK contract")
    permissions = re.findall(r"^uses-permission(?:-sdk-\d+)?: name='([^']+)'", text, re.M)
    if permissions != ["android.permission.INTERNET"]:
        raise ValueError("Unexpected APK permissions")
    debuggable = bool(re.search(r"^application-debuggable$", text, re.M))
    if debuggable != (variant == "debug"):
        raise ValueError("APK debuggable flag does not match requested variant")


def check_signature(returncode: int, text: str, variant: str, expected: str | None) -> str | None:
    if variant == "unsigned-release":
        if returncode == 0 or not re.search(r"Missing META-INF/MANIFEST.MF|no.*signatures|not signed", text, re.I):
            raise ValueError("Unsigned verification requires an actually unsigned APK")
        return None
    if returncode != 0:
        raise ValueError("APK signature verification failed")
    certs = re.findall(r"Signer #\d+ certificate SHA-256 digest: ([a-fA-F0-9]{64})", text)
    if len(certs) != 1:
        raise ValueError("Exactly one verified signing certificate is required")
    cert = certs[0].lower()
    if variant == "release":
        if not expected or not re.fullmatch(r"[a-fA-F0-9]{64}", expected):
            raise ValueError("Release packaging requires a trusted expected certificate SHA-256")
        if cert != expected.lower() or "CN=Android Debug" in text:
            raise ValueError("Release signer does not match the fixed release certificate")
    return cert


def tool(sdk: Path, name: str) -> Path:
    suffix = ".bat" if os.name == "nt" and name == "apksigner" else ".exe" if os.name == "nt" else ""
    result = sdk / "build-tools/35.0.0" / (name + suffix)
    if not result.is_file():
        raise ValueError(f"Missing Android build-tools 35.0.0 tool: {name}")
    return result


def run(command: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding="utf-8", errors="replace", check=False)


def source_info() -> dict:
    result = run(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    commit = result.stdout.strip()
    if result.returncode or not re.fullmatch(r"[a-f0-9]{40,64}", commit):
        raise ValueError("Build must run from a Git checkout with an exact source commit")
    status = run(["git", "-C", str(ROOT), "status", "--porcelain", "--untracked-files=normal", "--", "."])
    if status.returncode:
        raise ValueError("Cannot determine source tree state")
    return {"commit": commit, "dirty": bool(status.stdout.strip())}


def package(variant: str, output: Path, sdk: Path, expected: str | None) -> Path:
    name, code = version()
    build_variant = "debug" if variant == "debug" else "release"
    build = ROOT / f"app/build/outputs/apk/{build_variant}"
    metadata = json.loads((build / "output-metadata.json").read_text())
    elements = metadata.get("elements", [])
    if metadata.get("applicationId") != APPLICATION_ID or metadata.get("variantName") != build_variant or len(elements) != 1:
        raise ValueError("Unexpected APK output metadata")
    element = elements[0]
    if element.get("versionName") != name or element.get("versionCode") != code:
        raise ValueError("Stale build version; rebuild before packaging")
    apk = (build / element["outputFile"]).resolve()
    if apk.parent != build.resolve() or not apk.is_file() or not zipfile.is_zipfile(apk):
        raise ValueError("APK output must be a regular ZIP file within the build directory")
    badging = run([str(tool(sdk, "aapt")), "dump", "badging", str(apk)])
    if badging.returncode:
        raise ValueError("Cannot inspect APK manifest")
    check_badging(badging.stdout, variant, name, code)
    signature = run([str(tool(sdk, "apksigner")), "verify", "--verbose", "--print-certs", str(apk)])
    cert = check_signature(signature.returncode, signature.stdout, variant, expected)
    source = source_info()
    output.mkdir(parents=True, exist_ok=True)
    target = output / f"rpi-web-android-{name}-{variant}.apk"
    shutil.copyfile(apk, target)
    digest = hashlib.sha256(target.read_bytes()).hexdigest()
    target.with_suffix(".apk.sha256").write_text(f"{digest}  {target.name}\n", encoding="ascii")
    report = {"applicationId": APPLICATION_ID, "versionName": name, "versionCode": code,
              "variant": variant, "installable": cert is not None, "debuggable": variant == "debug",
              "minSdk": 26, "targetSdk": 35, "permissions": ["android.permission.INTERNET"],
              "source": source, "certificateSha256": cert, "sha256": digest, "bytes": target.stat().st_size,
              "defaultOrigin": "https://work.bigfisharc.cn", "client": "online-system-webview"}
    target.with_suffix(".json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Packaged {target.name}: {digest}")
    return target


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    ver = sub.add_parser("version")
    ver.add_argument("--tag")
    pack = sub.add_parser("package")
    pack.add_argument("--variant", choices=["debug", "release", "unsigned-release"], required=True)
    pack.add_argument("--output", type=Path, default=ROOT / "dist")
    pack.add_argument("--sdk", type=Path, default=None)
    args = parser.parse_args()
    if args.command == "version":
        name, _ = version()
        if args.tag:
            check_tag(args.tag, name)
        print(name)
    else:
        sdk = args.sdk or Path(os.environ.get("ANDROID_SDK_ROOT") or os.environ.get("ANDROID_HOME") or Path.home() / "Android/Sdk")
        package(args.variant, args.output.resolve(), sdk, os.environ.get("RPI_WEB_ANDROID_CERT_SHA256"))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, json.JSONDecodeError) as error:
        print(f"Android packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
