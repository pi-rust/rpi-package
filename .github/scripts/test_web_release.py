import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("web_release", Path(__file__).with_name("web-release.py"))
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)


class WebReleaseTests(unittest.TestCase):
    def test_missing_frontend_resources_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "index.html").write_text('<script src="/assets/app.js"></script>')
            with self.assertRaises(ValueError):
                web.frontend_inventory(root)
            (root / "assets").mkdir()
            (root / "assets/app.js").write_text("web")
            self.assertEqual(set(web.frontend_inventory(root)), {"index.html", "assets/app.js"})

    def test_all_platforms_must_share_source_and_frontend_before_upload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            args = argparse.Namespace(output=directory, source_ref="a" * 40,
                                      tag="rpi-web-v0.1.0", repository="pi-rust/rpi-package")
            fragments = []
            for target in sorted(web.assets.TARGETS):
                path = root / f"{target}.zip"
                path.write_bytes(target.encode())
                fragment = dict(schema_version=1, name="rpi-web", version="0.1.0", tag=args.tag,
                                repository="https://github.com/pi-rust/rpi-package", source_commit=args.source_ref,
                                target=target, frontend={"index.html": "same"},
                                distribution=web.assets.asset(path, args.repository, args.tag))
                fragments.append(fragment)
                web.assets.write_json(root / f"rpi-web.{target}.json", fragment)
            with mock.patch.object(web.assets, "check_tag_source"), \
                    mock.patch.object(web.assets, "command", return_value='{"assets":[]}'), \
                    mock.patch.object(web.subprocess, "run") as upload:
                web.publish(args)
                self.assertEqual(len(upload.call_args_list), 2)
                self.assertIn("rpi-web-release.json", upload.call_args_list[-1].args[0][-1])
                fragments[0]["frontend"] = {"index.html": "different"}
                web.assets.write_json(root / f"rpi-web.{fragments[0]['target']}.json", fragments[0])
                upload.reset_mock()
                with self.assertRaises(ValueError):
                    web.publish(args)
                upload.assert_not_called()

    def test_missing_platform_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=directory, source_ref="a" * 40)
            with self.assertRaises(ValueError):
                web.publish(args)

    def test_web_tag_selection_does_not_enter_extension_pipeline(self):
        spec = importlib.util.spec_from_file_location("tags", Path(__file__).with_name("select-release-tags.py"))
        tags = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(tags)
        metadata = {"workspace_members": ["web"], "packages": [dict(id="web", name="rpi-web", version="0.1.0",
                    publish=None, targets=[{"crate_types": ["bin"]}])]}
        refs = {"refs/tags/rpi-web-v0.1.0": "tag", "refs/tags/rpi-web-v0.1.0^{}": "commit"}
        self.assertEqual(tags.select_web(metadata, refs, "commit"), "rpi-web-v0.1.0")
        self.assertEqual(tags.select(metadata, refs, "commit"), [])
        self.assertEqual(tags.select_web(metadata, refs, "other"), "")
        metadata["packages"][0]["targets"].append({"crate_types": ["cdylib", "rlib"]})
        self.assertEqual(tags.select(metadata, refs, "commit"), [])
        self.assertEqual(tags.select_web(metadata, refs, "commit"), "rpi-web-v0.1.0")


if __name__ == "__main__":
    unittest.main()
