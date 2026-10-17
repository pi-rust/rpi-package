"""Catalog updates must not invent releases for source-only provider packages."""
import json
import os
import pathlib
import runpy
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = pathlib.Path(__file__).with_name("sync-release-catalog.py")


class SyncReleaseCatalogTests(unittest.TestCase):
    def test_source_only_entry_keeps_local_install_and_has_no_release_url(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "catalog").mkdir()
            catalog = root / "catalog/packages.json"
            catalog.write_text(json.dumps([
                {"name": "provider", "install": "rpi package link deepseek --path packages/provider", "prebuilt_manifest": "stale"},
                {"name": "published", "install": "rpi install published"},
                {"name": "prebuilt-only", "install": "rpi install prebuilt-only --github example/packages"},
            ]), encoding="utf-8")
            metadata = {"packages": [
                {"name": "provider", "version": "0.1.0", "publish": [], "repository": "https://github.com/example/packages"},
                {"name": "published", "version": "1.2.3", "publish": None, "repository": "https://github.com/example/packages"},
                {"name": "prebuilt-only", "version": "0.1.0", "publish": [], "repository": "https://github.com/example/packages", "metadata": {"rpi": {"prebuilt": True}}},
            ]}
            previous = pathlib.Path.cwd()
            try:
                os.chdir(root)
                with patch("subprocess.check_output", return_value=json.dumps(metadata).encode()):
                    runpy.run_path(str(SCRIPT), run_name="__main__")
            finally:
                os.chdir(previous)
            source, published, prebuilt = json.loads(catalog.read_text(encoding="utf-8"))
            self.assertEqual(source["version"], "0.1.0")
            self.assertTrue(source["install"].startswith("rpi package link"))
            self.assertNotIn("prebuilt_manifest", source)
            self.assertEqual(published["prebuilt_manifest"], "https://github.com/example/packages/releases/download/published-v1.2.3/rpi-extension.json")
            self.assertEqual(prebuilt["prebuilt_manifest"], "https://github.com/example/packages/releases/download/prebuilt-only-v0.1.0/rpi-extension.json")
            self.assertEqual(prebuilt["install"], "rpi install prebuilt-only --github example/packages")


if __name__ == "__main__":
    unittest.main()
