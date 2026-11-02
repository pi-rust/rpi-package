#!/usr/bin/env python3
"""Release-only native UI smoke on a disposable emulator. No CDP, credentials or model requests.
Install the signed APK first. The script switches site preferences in this test app.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess
import time
import xml.etree.ElementTree as ET

from package_apk import APPLICATION_ID, ROOT, version
from env_config import load_environment, DEFAULT, DEVELOPMENT


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"emulator-\d+", args.serial):
        raise SystemExit("This test is restricted to disposable emulators, not user phones")
    environment = load_environment()
    production = environment[DEFAULT]
    development = environment[DEVELOPMENT]
    if not development:
        raise SystemExit("Full release smoke requires the optional development origin")
    out = ROOT / "artifacts"
    out.mkdir(exist_ok=True)
    checks = []

    def adb(*command, check=True):
        return subprocess.run(["adb", "-s", args.serial, *command], capture_output=True, encoding="utf-8", errors="replace", timeout=30, check=check)

    def ok(text):
        checks.append(text)
        print("PASS", text, flush=True)

    def dump():
        adb("shell", "uiautomator", "dump", "/sdcard/rpi-release-ui.xml", check=False)
        xml = adb("shell", "cat", "/sdcard/rpi-release-ui.xml").stdout
        return ET.fromstring(xml)

    def wait(predicate, seconds=25):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ui = dump()
            if predicate(ui):
                return ui
            time.sleep(.5)
        raise AssertionError("Native UI condition timed out")

    def node(ui, text):
        return next((n for n in ui.iter("node") if n.get("text") == text or n.get("content-desc") == text), None)

    def tap(text):
        ui = wait(lambda ui: node(ui, text) is not None)
        match = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", node(ui, text).get("bounds", ""))
        assert match, "Missing UI bounds"
        x1, y1, x2, y2 = map(int, match.groups())
        adb("shell", "input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2))
        time.sleep(.4)

    def site(label, origin):
        tap("站点设置")
        tap(label)
        tap("切换")
        wait(lambda ui: node(ui, "当前站点：" + origin) is not None)
        wait(lambda ui: any(n.get("password") == "true" for n in ui.iter("node")))

    def start():
        adb("shell", "am", "force-stop", APPLICATION_ID)
        result = adb("shell", "am", "start", "-W", "-n", APPLICATION_ID + "/.MainActivity")
        assert "Status: ok" in result.stdout, result.stdout

    name, code = version()
    package = adb("shell", "dumpsys", "package", APPLICATION_ID).stdout
    assert f"versionName={name}" in package and re.search(rf"versionCode={code}\b", package)
    flags = re.findall(r"(?:pkgFlags|flags)=\[([^]]*)\]", package)
    assert flags and not any("DEBUGGABLE" in item.split() for item in flags)
    run_as = adb("shell", "run-as", APPLICATION_ID, "id", check=False)
    assert run_as.returncode != 0 and "not debuggable" in (run_as.stdout + run_as.stderr)
    ok("Installed release has expected version and is not debuggable (run-as denied)")
    start()
    wait(lambda ui: node(ui, "当前站点：" + production) is not None)
    ui = wait(lambda ui: any(n.get("password") == "true" for n in ui.iter("node")))
    ok("Release loads real production HTTPS login without authentication")
    # The app requests debugging=false. WebView 113 on ro.debuggable=1 system
    # images deliberately ignores this API (SharedStatics.java); do not claim
    # absence of CDP on that emulator, or use it to drive this release smoke.
    pid = adb("shell", "pidof", APPLICATION_ID).stdout.strip().split()[0]
    sockets = adb("shell", "cat", "/proc/net/unix").stdout
    observed_socket = bool(re.search(r"@webview_devtools_remote_" + re.escape(pid) + r"$", sockets, re.M))
    debug_os = adb("shell", "getprop", "ro.debuggable").stdout.strip() == "1"
    if not debug_os:
        assert not observed_socket, "Release WebView must not expose CDP on a non-debug system"
    print("INFO WebView debug socket:", observed_socket, "; system ro.debuggable:", debug_os, "; CDP not used", flush=True)

    password = next(n for n in ui.iter("node") if n.get("password") == "true")
    x1, y1, x2, y2 = map(int, re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", password.get("bounds")).groups())
    adb("shell", "input", "tap", str((x1 + x2) // 2), str((y1 + y2) // 2))
    time.sleep(1)
    assert re.search(r"mInputShown=true|isInputViewShown=true|mIsInputViewShown=true", adb("shell", "dumpsys", "input_method").stdout)
    adb("shell", "input", "keyevent", "4")
    wait(lambda ui: node(ui, "站点设置") is not None)
    ok("Native keyboard opens on login; Back dismisses it without leaving release app")
    site("开发站（测试界面）", development)
    ok("Native site picker loads real development HTTPS login")
    start()
    wait(lambda ui: node(ui, "当前站点：" + development) is not None)
    wait(lambda ui: any(n.get("password") == "true" for n in ui.iter("node")))
    ok("Selected HTTPS site persists across release process restart")
    site("正式站", production)
    ok("Native site picker restores production default")
    pid_before = adb("shell", "pidof", APPLICATION_ID).stdout.strip()
    try:
        tap("站点设置")
        adb("shell", "cmd", "uimode", "night", "yes")
        time.sleep(1)
        assert adb("shell", "pidof", APPLICATION_ID).stdout.strip() == pid_before
        wait(lambda ui: node(ui, "自定义 HTTPS 站点") is not None)
        image = subprocess.run(["adb", "-s", args.serial, "exec-out", "screencap", "-p"], capture_output=True, timeout=30, check=True)
        (out / "release-night-dialog.png").write_bytes(image.stdout)
        tap("取消")
        start()
        wait(lambda ui: any(n.get("password") == "true" for n in ui.iter("node")))
        image = subprocess.run(["adb", "-s", args.serial, "exec-out", "screencap", "-p"], capture_output=True, timeout=30, check=True)
        (out / "release-night-start.png").write_bytes(image.stdout)
        ok("Release handles live OS night changes with open dialog and cold-starts in night mode")
    finally:
        adb("shell", "cmd", "uimode", "night", "no")
    wait(lambda ui: node(ui, "当前站点：" + production) is not None)
    (out / "release-ui.xml").write_text(ET.tostring(dump(), encoding="unicode"), encoding="utf-8")
    image = subprocess.run(["adb", "-s", args.serial, "exec-out", "screencap", "-p"], capture_output=True, timeout=30, check=True)
    (out / "release-production.png").write_bytes(image.stdout)
    report = {"status": "passed", "variant": "release", "serial": args.serial,
              "android": adb("shell", "getprop", "ro.build.version.release").stdout.strip(), "checks": checks,
              "authentication": "not attempted", "realModelCalls": 0, "CDP": "not used",
              "webviewDebugging": {"appRequested": False, "systemDebuggable": debug_os, "observedSocket": observed_socket,
                                    "limitation": "WebView 113 on debug Android forces remote debugging; normal non-debug device behavior not verified here" if debug_os else None},
              "scope": "Native shell and unauthenticated login only; document picker separately verified using debug/CDP fixture"}
    (out / "release-emulator-report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
