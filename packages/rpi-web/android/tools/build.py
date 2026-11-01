#!/usr/bin/env python3
"""Local cross-platform Android build; credentials are environment-only and never printed."""
import argparse
import os
from pathlib import Path
import subprocess
import sys

from package_apk import ROOT, package


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--variant", choices=["debug", "release", "unsigned-release"], default="debug")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--sdk", type=Path)
    args = parser.parse_args()
    sdk = args.sdk or Path(os.environ.get("ANDROID_SDK_ROOT") or os.environ.get("ANDROID_HOME") or Path.home() / "Android/Sdk")
    if not (sdk / "platforms/android-35/android.jar").is_file():
        raise SystemExit("Set ANDROID_SDK_ROOT or --sdk to an SDK with Android platform 35 and build-tools 35.0.0")
    env = os.environ | {"ANDROID_HOME": str(sdk.resolve())}
    if args.variant == "release":
        for suffix in ("KEYSTORE_PATH", "STORE_PASSWORD", "KEY_ALIAS", "KEY_PASSWORD", "CERT_SHA256"):
            if not env.get("RPI_WEB_ANDROID_" + suffix):
                raise SystemExit("Missing release signing environment variable: RPI_WEB_ANDROID_" + suffix)
    elif any(env.get("RPI_WEB_ANDROID_" + name) for name in ("KEYSTORE_PATH", "STORE_PASSWORD", "KEY_ALIAS", "KEY_PASSWORD")):
        raise SystemExit("Debug/unsigned builds must not receive release signing credentials")
    artifacts = ROOT / "artifacts"
    artifacts.mkdir(exist_ok=True)
    subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tools", "-p", "test_*.py", "-v"], cwd=ROOT, check=True)
    variant = "Debug" if args.variant == "debug" else "Release"
    command = [str(ROOT / ("gradlew.bat" if os.name == "nt" else "gradlew")), "--no-daemon"]
    if args.offline:
        command.append("--offline")
    if args.variant == "unsigned-release":
        command.append("-PallowUnsignedRelease=true")
    command.extend([f"test{variant}UnitTest", f"lint{variant}", f"assemble{variant}"])
    log = artifacts / f"build-{args.variant}.log"
    with log.open("w", encoding="utf-8") as output:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=output, stderr=subprocess.STDOUT)
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines()[-15:]:
        print(line)
    if result.returncode:
        raise SystemExit(f"Gradle failed; see {log}")
    package(args.variant, ROOT / "dist" / args.variant, sdk.resolve(), env.get("RPI_WEB_ANDROID_CERT_SHA256"))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Android build failed: {error}", file=sys.stderr)
        sys.exit(1)
