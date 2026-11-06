"""Qualify actual Cargo source selection and temporary patch lock restoration."""
from pathlib import Path
import subprocess
import sys
import unittest


class WorkspaceCargoTests(unittest.TestCase):
    root = Path(__file__).resolve().parents[2]

    def run_cargo(self, *arguments):
        return subprocess.run([sys.executable, str(self.root / '.github/scripts/workspace-cargo.py'),
                               *arguments], cwd=self.root, capture_output=True, text=True,
                              encoding='utf-8')

    def test_workspace_identity_is_selected_over_registry_names(self):
        result = self.run_cargo('pkgid', '-p', 'rpi-extcore')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('/packages/rpi-extcore#', result.stdout)

    def test_patch_graph_restores_lock_on_success_and_child_failure(self):
        lock = self.root / 'Cargo.lock'
        original = lock.read_bytes()
        patch = ['--config', 'patch.crates-io.rpi-durable-core.path="packages/rpi-durable-core"']
        for name, expected in [('rpi-durable', 0), ('nonexistent-package-fixture', 101)]:
            with self.subTest(package=name):
                result = self.run_cargo('pkgid', '-p', name, *patch)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(lock.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
