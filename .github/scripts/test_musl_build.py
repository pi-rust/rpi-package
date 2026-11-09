"""Exercise package selection and missing-artifact rejection in the shell helper."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires POSIX bash")
class MuslBuildTests(unittest.TestCase):
    def test_all_selected_packages_are_built_and_missing_library_fails(self):
        helper = Path(__file__).resolve().parents[2] / "tools/build-musl.sh"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / "bin"
            binaries.mkdir()
            release = root / "target/release"
            release.mkdir(parents=True)
            packages = []
            for name in ["first", "second"]:
                packages.append({"id": name, "name": name, "version": "1.0.0",
                    "targets": [{"name": name, "crate_types": ["cdylib"]}],
                    "metadata": {"rpi": {"prebuilt": {"supported-targets": [
                        "x86_64-unknown-linux-musl"]}}}})
                (release / f"lib{name}.so").touch()
            (root / "metadata.json").write_text(json.dumps({"packages": packages,
                "workspace_members": [p["id"] for p in packages],
                "target_directory": str(root / "target")}))

            def executable(name, body):
                path = binaries / name
                path.write_text("#!/bin/sh\nset -eu\n" + body)
                path.chmod(0o755)
                return str(path)

            executable("rustc", "echo 'host: x86_64-unknown-linux-musl'\n")
            executable("cargo", 'if [ "$1" = metadata ]; then cat "$PROJECT_ROOT/metadata.json"; '
                'else printf "%s\\n" "$@" > "$PROJECT_ROOT/build-args"; fi\n')
            compiler = executable("fixture-cc", 'while [ "$1" != -o ]; do shift; done\n'
                'shift\nprintf "#!/bin/sh\\nexit 0\\n" > "$1"\nchmod +x "$1"\n')
            executable("readelf", "exit 0\n")
            executable("nm", "echo '0000 T rpi_plugin_register'\n")
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                PROJECT_ROOT=str(root), CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=compiler)
            for key in ["SOURCE_REF", "PACKAGE_RELEASES", "PACKAGES_ONLY", "TARGET", "CC"]:
                env.pop(key, None)
            result = subprocess.run(["bash", str(helper)], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / "build-args").read_text().splitlines(),
                ["build", "--release", "--locked", "--lib", "--package", "first", "--package", "second"])
            (release / "libsecond.so").unlink()
            result = subprocess.run(["bash", str(helper)], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing selected musl library:", result.stderr)
            self.assertIn("libsecond.so", result.stderr)


if __name__ == "__main__":
    unittest.main()
