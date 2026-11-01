#!/usr/bin/env python3
"""Unit checks for package rejection paths; no SDK, signing keys or network required."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("package_apk", Path(__file__).with_name("package_apk.py"))
apk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(apk)
CERT = "ab" * 32
BADGING = """package: name='cn.bigfisharc.rpiweb.local' versionCode='2' versionName='0.1.1'
sdkVersion:'26'
targetSdkVersion:'35'
uses-permission: name='android.permission.INTERNET'
"""
SIGNATURE = f"Verifies\nSigner #1 certificate DN: CN=rpi Web Android Release\nSigner #1 certificate SHA-256 digest: {CERT}\n"


class PackageContract(unittest.TestCase):
    def test_release_manifest(self):
        apk.check_badging(BADGING, "release", "0.1.1", 2)
        apk.check_badging(BADGING, "unsigned-release", "0.1.1", 2)

    def test_debug_requires_debuggable(self):
        with self.assertRaises(ValueError):
            apk.check_badging(BADGING, "debug", "0.1.1", 2)
        apk.check_badging(BADGING + "application-debuggable\n", "debug", "0.1.1", 2)

    def test_rejects_wrong_identity_version_sdk_permissions(self):
        for text in [BADGING.replace("cn.bigfisharc.rpiweb.local", "another.app"),
                     BADGING.replace("versionCode='2'", "versionCode='1'"),
                     BADGING.replace("versionName='0.1.1'", "versionName='0.1.0'"),
                     BADGING.replace("sdkVersion:'26'", "sdkVersion:'25'"),
                     BADGING.replace("targetSdkVersion:'35'", "targetSdkVersion:'34'"),
                     BADGING + "uses-permission: name='android.permission.CAMERA'\n",
                     BADGING + "uses-permission-sdk-23: name='android.permission.READ_EXTERNAL_STORAGE'\n"]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                apk.check_badging(text, "release", "0.1.1", 2)

    def test_release_rejects_debuggable(self):
        with self.assertRaises(ValueError):
            apk.check_badging(BADGING + "application-debuggable\n", "release", "0.1.1", 2)

    def test_fixed_certificate(self):
        self.assertEqual(apk.check_signature(0, SIGNATURE, "release", CERT.upper()), CERT)

    def test_missing_wrong_debug_or_multiple_certificate_rejected(self):
        cases = [(1, "DOES NOT VERIFY", CERT), (0, SIGNATURE, None),
                 (0, SIGNATURE, "cd" * 32), (0, SIGNATURE, "invalid"),
                 (0, SIGNATURE.replace("CN=rpi Web Android Release", "CN=Android Debug"), CERT),
                 (0, SIGNATURE + f"Signer #2 certificate SHA-256 digest: {CERT}\n", CERT)]
        for code, text, expected in cases:
            with self.subTest(code=code, expected=expected), self.assertRaises(ValueError):
                apk.check_signature(code, text, "release", expected)

    def test_unsigned_is_explicit_and_not_a_signed_or_broken_tool_result(self):
        self.assertIsNone(apk.check_signature(1, "DOES NOT VERIFY\nERROR: Missing META-INF/MANIFEST.MF", "unsigned-release", None))
        for code, text in [(0, SIGNATURE), (1, "java not found"), (2, "cannot open APK")]:
            with self.assertRaises(ValueError):
                apk.check_signature(code, text, "unsigned-release", None)

    def test_debug_still_requires_signature(self):
        self.assertEqual(apk.check_signature(0, SIGNATURE, "debug", None), CERT)
        with self.assertRaises(ValueError):
            apk.check_signature(1, SIGNATURE, "debug", None)

    def test_tag_exact_match(self):
        apk.check_tag("rpi-web-android-v0.1.1", "0.1.1")
        for tag in ["rpi-web-v0.1.1", "rpi-web-android-v0.1.0", "rpi-web-android-v0.1.1-extra", "v0.1.1"]:
            with self.assertRaises(ValueError):
                apk.check_tag(tag, "0.1.1")

    def test_version_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            file = root / "version.properties"
            file.write_text("versionName=0.1.1\nversionCode=2\n")
            self.assertEqual(apk.version(root), ("0.1.1", 2))
            for text in ["versionName=../bad\nversionCode=2", "versionName=0.1.1\nversionCode=0", "versionName=0.1.1\nversionCode=2100000001"]:
                file.write_text(text)
                with self.assertRaises(ValueError):
                    apk.version(root)


class SourceSelectionContract(unittest.TestCase):
    """Mock Git only; these tests never fetch, checkout, or touch signing material."""
    def setUp(self):
        from unittest.mock import patch
        import os
        script = Path(__file__).resolve().parents[4] / ".github/scripts/resolve-android-source.py"
        spec = importlib.util.spec_from_file_location("android_source", script)
        self.source = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.source)
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.output = Path(self.directory.name) / "output"
        self.commit = "a" * 40
        self.env = {"GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF": "refs/heads/main",
                    "GITHUB_SHA": self.commit, "GITHUB_OUTPUT": str(self.output)}
        env_patch = patch.dict(os.environ, self.env, clear=True)
        env_patch.start()
        self.addCleanup(env_patch.stop)
        self.calls = []

        def git(*args):
            self.calls.append(args)
            if args[0] == "rev-parse":
                return self.commit
            if args[0] == "show":
                return "versionName=0.1.1\nversionCode=2"
            return ""
        git_patch = patch.object(self.source, "git", side_effect=git)
        self.mock_git = git_patch.start()
        self.addCleanup(git_patch.stop)

    def invoke(self, tag=""):
        from unittest.mock import patch
        with patch("sys.argv", ["resolve-android-source.py", "--tag", tag]):
            self.source.main()

    def test_main_artifact_selects_exact_workflow_commit(self):
        self.invoke()
        self.assertIn(("merge-base", "--is-ancestor", self.commit, "origin/main"), self.calls)
        self.assertIn(("checkout", "--detach", self.commit), self.calls)
        self.assertEqual(self.output.read_text(), f"source_commit={self.commit}\n")

    def test_tag_checks_ancestry_and_version_before_checkout(self):
        self.invoke("rpi-web-android-v0.1.1")
        ancestor = ("merge-base", "--is-ancestor", self.commit, "origin/main")
        checkout = ("checkout", "--detach", self.commit)
        self.assertLess(self.calls.index(ancestor), self.calls.index(checkout))
        self.assertTrue(any(call[0] == "show" for call in self.calls))

    def test_non_main_and_pr_never_reach_git(self):
        from unittest.mock import patch
        import os
        for env in [{"GITHUB_EVENT_NAME": "pull_request"}, {"GITHUB_REF": "refs/heads/feature"},
                    {"GITHUB_REF": "refs/tags/rpi-web-android-v0.1.1"}]:
            with patch.dict(os.environ, env), self.assertRaises(SystemExit):
                self.invoke()
        self.assertFalse(self.calls)

    def test_malformed_tag_never_reaches_git(self):
        for tag in ["main", "--help", "rpi-web-android-v0.1.1\nsource_commit=bad", "rpi-web-android-v0.1.1;echo bad"]:
            with self.assertRaises(SystemExit):
                self.invoke(tag)
        self.assertFalse(self.calls)

    def test_version_mismatch_never_checks_out(self):
        with self.assertRaises(SystemExit):
            self.invoke("rpi-web-android-v0.1.0")
        self.assertFalse(any(call[0] == "checkout" for call in self.calls))
        self.assertFalse(self.output.exists())

    def test_non_ancestor_never_checks_out(self):
        import subprocess
        previous = self.mock_git.side_effect
        def reject(*args):
            if args[0] == "merge-base":
                raise subprocess.CalledProcessError(1, ["git", *args])
            return previous(*args)
        self.mock_git.side_effect = reject
        with self.assertRaises(subprocess.CalledProcessError):
            self.invoke("rpi-web-android-v0.1.1")
        self.assertFalse(any(call[0] == "checkout" for call in self.calls))
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
