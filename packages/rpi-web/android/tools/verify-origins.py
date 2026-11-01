#!/usr/bin/env python3
"""Disposable debug-emulator endpoint smoke. No login/CDP/model calls; reset is explicit.
Run --reset on an installed build, then install a build with a changed default and
run --preserve to verify the fixture site preference survives a build configuration change.
"""
import argparse
import re
import subprocess
import time
import xml.etree.ElementTree as ET

from env_config import DEFAULT, DEVELOPMENT, load_environment
from package_apk import APPLICATION_ID

SAVED_FIXTURE = "https://saved.example"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--reset", action="store_true", help="Clear this test app's local data first")
    mode.add_argument("--preserve", action="store_true", help="Expect the fixture preference saved by a previous --reset run")
    args = parser.parse_args()
    if not re.fullmatch(r"emulator-\d+", args.serial):
        parser.error("Only disposable emulator serials are allowed")
    config = load_environment()

    def adb(*command):
        return subprocess.run(["adb", "-s", args.serial, *command], capture_output=True, encoding="utf-8", errors="replace", timeout=30, check=True).stdout

    def ui():
        adb("shell", "uiautomator", "dump", "/sdcard/rpi-origin-ui.xml")
        return ET.fromstring(adb("shell", "cat", "/sdcard/rpi-origin-ui.xml"))

    def node(tree, text):
        return next((n for n in tree.iter("node") if n.get("text") == text or n.get("content-desc") == text), None)

    def wait(text):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            found = node(ui(), text)
            if found is not None:
                return found
            time.sleep(.3)
        raise AssertionError("Expected native UI state not reached (endpoint values redacted)")

    def tap(text):
        found = wait(text)
        bounds = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", found.get("bounds", ""))
        assert bounds, "Native UI bounds missing"
        x1, y1, x2, y2 = map(int, bounds.groups())
        adb("shell", "input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2))
        time.sleep(.3)

    def start():
        adb("shell", "am", "force-stop", APPLICATION_ID)
        adb("shell", "am", "start", "-W", "-n", APPLICATION_ID + "/.MainActivity")

    # Refuse release apps: --reset is only for disposable debug smoke state.
    adb("shell", "run-as", APPLICATION_ID, "id")
    if args.reset:
        adb("shell", "pm", "clear", APPLICATION_ID)
    start()
    wait("当前站点：" + (SAVED_FIXTURE if args.preserve else config[DEFAULT]))
    print("PASS startup uses configured default or retained fixture preference", flush=True)
    tap("站点设置")
    tree = ui()
    assert (node(tree, "开发站（测试界面）") is not None) == bool(config[DEVELOPMENT]), "Optional development menu visibility mismatch"
    print("PASS development menu presence follows optional build setting", flush=True)
    tap("取消")
    if args.reset:
        # Seed ONLY this disposable debug app's preference via run-as, avoiding
        # IME locale transformations of a URL typed by adb input text.
        adb("shell", "am", "force-stop", APPLICATION_ID)
        command = f"run-as {APPLICATION_ID} sh -c 'mkdir -p shared_prefs && cat > shared_prefs/connection.xml'"
        subprocess.run(["adb", "-s", args.serial, "shell", command],
                       input=f'<map><string name="origin">{SAVED_FIXTURE}</string></map>',
                       capture_output=True, encoding="utf-8", timeout=30, check=True)
    start()
    wait("当前站点：" + SAVED_FIXTURE)
    print("PASS saved fixture site preference survives process restart", flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception:
        raise SystemExit("Endpoint smoke failed; inspect native UI locally (endpoint values redacted)") from None
