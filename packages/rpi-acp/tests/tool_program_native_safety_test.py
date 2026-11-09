#!/usr/bin/env python3
"""Focused stdlib-only safety tests. Never starts bridge/native or OS cleanup.

Run: python -B packages/rpi-acp/tests/tool_program_native_safety_test.py
"""
import ast
import importlib.util
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "tool_program_native", Path(__file__).with_name("tool_program_native.py"))
harness = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(harness)


class FakeWindows:
    def __init__(self):
        self.objects, self.by_pid, self.rows = {}, {}, {}
        self.opened, self.closed, self.terminated = [], [], []
        self.on_open = None
        self.on_snapshot = None
        self.snapshots = 0

    def add(self, handle, pid, parent, created, image, alive=True):
        self.objects[handle] = dict(identity=dict(pid=pid, creationTime100ns=created,
            imagePath=str(image)), alive=alive, exitCode=259 if alive else 0)
        self.by_pid[pid] = handle
        self.rows[pid] = dict(pid=pid, parentPid=parent, executable=Path(image).name)

    def duplicate(self, handle):
        return handle

    def open(self, pid):
        self.opened.append(pid)
        if self.on_open:
            self.on_open(pid)
        return self.by_pid[pid]

    def kernel_key(self, handle):
        return {key: self.objects[handle]["identity"][key]
                for key in ("pid", "creationTime100ns")}

    def identity(self, handle):
        return dict(self.objects[handle]["identity"])

    def state(self, handle):
        obj = self.objects[handle]
        return dict(alive=obj["alive"], exitCode=obj["exitCode"])

    def snapshot(self):
        self.snapshots += 1
        if self.on_snapshot:
            self.on_snapshot(self.snapshots)
        return 1000, {pid: dict(row) for pid, row in self.rows.items()}

    def terminate(self, handle):
        self.terminated.append(handle)
        self.objects[handle].update(alive=False, exitCode=1)

    def close(self, handle):
        self.closed.append(handle)


class SafetyTest(unittest.TestCase):
    def setUp(self):
        # Guard against accidentally turning these tests into acceptance runs.
        for name in ("Popen", "run"):
            guard = patch.object(harness.subprocess, name,
                                 side_effect=AssertionError("no real process in unit tests"))
            guard.start()
            self.addCleanup(guard.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.paths = {}
        for role in ("bridge", "launcher", "native"):
            path = self.directory / (role + ".exe")
            path.write_bytes(role.encode())
            self.paths[role] = path.resolve()
        self.receipt = self.directory / "sessions" / "case" / "native-pid.txt"
        self.receipt.parent.mkdir(parents=True)
        self.api = FakeWindows()
        self.api.add("bridge-h", 10, 1, 100, self.paths["bridge"])
        self.api.add("launcher-h", 20, 10, 200, self.paths["launcher"])
        self.api.add("native-h", 30, 20, 300, self.paths["native"])
        self.registry = harness.Processes(SimpleNamespace(pid=10, _handle="bridge-h"),
            self.paths["bridge"], self.paths["launcher"], self.paths["native"],
            self.receipt, api=self.api)
        self.addCleanup(self.registry.close)

    def chain(self):
        self.receipt.write_text("30")
        self.registry.capture()
        self.assertEqual(set(self.registry.rows), {"bridge", "launcher", "native"})

    def reject_launcher(self):
        with self.assertRaises(AssertionError):
            self.registry.capture()
        self.assertNotIn("launcher", self.registry.handles)
        self.assertFalse(self.api.terminated)

    def test_exact_chain_and_kernel_audit(self):
        self.chain()
        rows = {row["role"]: row for row in self.registry.status()}
        self.assertEqual(rows["native"]["creationTime100ns"], 300)
        self.assertEqual(rows["native"]["parentCreationTime100ns"], 200)
        self.assertEqual(rows["native"]["imagePath"], str(self.paths["native"]))
        self.assertEqual(rows["native"]["imageSha256"], harness.sha(self.paths["native"]))
        self.assertEqual(self.api.opened, [20, 30])  # Never reopen root by PID.
        snapshots = self.api.snapshots
        self.registry.capture()
        self.assertEqual(self.api.snapshots, snapshots)  # No ownership expansion.

    def test_exited_retained_object_can_lose_only_its_image_name(self):
        self.chain()
        self.api.objects["native-h"].update(alive=False, exitCode=0)
        self.api.objects["native-h"]["identity"]["imagePath"] = None
        native = next(row for row in self.registry.status() if row["role"] == "native")
        self.assertFalse(native["alive"])
        self.assertEqual(native["imagePath"], str(self.paths["native"]))
        self.assertEqual(self.registry.terminate_known()[-1]["role"], "bridge")
        self.assertNotIn("native-h", self.api.terminated)

    def test_postregistration_image_section_loss_never_reopens_by_pid(self):
        self.chain()
        self.api.objects["native-h"]["identity"]["imagePath"] = None
        self.api.add("external-new-native", 30, 999, 1100, self.paths["native"])
        self.registry.terminate_known()
        self.assertEqual(self.api.opened, [20, 30])
        self.assertTrue(self.api.objects["external-new-native"]["alive"])
        self.assertEqual(self.api.terminated, ["native-h", "launcher-h", "bridge-h"])

    def test_dead_parent_pid_reused(self):
        self.api.objects["bridge-h"].update(alive=False, exitCode=0)
        self.api.add("external-root", 10, 1, 400, self.paths["bridge"])
        self.reject_launcher()
        self.assertFalse(self.api.opened)

    def test_child_predates_parent(self):
        self.api.objects["launcher-h"]["identity"]["creationTime100ns"] = 99
        self.reject_launcher()
        self.assertIn("launcher-h", self.api.closed)

    def test_image_mismatch_same_basename(self):
        other = self.directory / "external" / "launcher.exe"
        self.api.objects["launcher-h"]["identity"]["imagePath"] = str(other)
        self.reject_launcher()

    def test_frozen_image_sha_change(self):
        self.paths["launcher"].write_bytes(b"changed")
        self.reject_launcher()

    def test_snapshot_open_new_process_same_pid(self):
        def replace(pid):
            self.api.add("replacement-h", pid, 10, 1001, self.paths["launcher"])
        self.api.on_open = replace
        self.reject_launcher()
        self.assertIn("replacement-h", self.api.closed)

    def test_snapshot_open_parent_change(self):
        self.api.on_open = lambda pid: self.api.rows[pid].update(parentPid=777)
        self.reject_launcher()

    def test_parent_exits_during_open(self):
        self.api.on_open = lambda pid: self.api.objects["bridge-h"].update(alive=False)
        self.reject_launcher()

    def test_ambiguous_launchers(self):
        self.api.add("second-launcher", 21, 10, 250, self.paths["launcher"])
        self.reject_launcher()
        self.assertFalse(self.api.opened)

    def test_conhost_is_never_an_ownership_root(self):
        self.api.rows.pop(20)
        self.api.add("console-h", 40, 10, 200, self.directory / "conhost.exe")
        self.api.add("outsider-h", 50, 40, 300, self.paths["launcher"])
        self.api.add("external-rpi", 60, 40, 350, self.paths["native"])
        self.registry.capture()
        self.assertEqual(set(self.registry.handles), {"bridge"})
        self.assertFalse(self.api.opened)
        self.registry.terminate_known()
        self.assertEqual(self.api.terminated, ["bridge-h"])

    def test_native_requires_explicit_receipt(self):
        self.registry.capture()
        self.assertEqual(set(self.registry.handles), {"bridge", "launcher"})
        self.assertNotIn(30, self.api.opened)

    def test_native_requires_exact_direct_parent(self):
        self.receipt.write_text("30")
        self.api.rows[30]["parentPid"] = 40  # Even a console intermediary is denied.
        with self.assertRaisesRegex(AssertionError, "not a direct child"):
            self.registry.capture()
        self.assertNotIn("native", self.registry.handles)
        self.assertNotIn(30, self.api.opened)

    def test_dead_launcher_pid_reuse_cannot_admit_native(self):
        self.registry.capture()
        self.api.objects["launcher-h"].update(alive=False, exitCode=0)
        self.api.add("replacement-launcher", 20, 10, 400, self.paths["launcher"])
        self.receipt.write_text("30")
        with self.assertRaisesRegex(AssertionError, "retained live object"):
            self.registry.capture()
        self.assertNotIn(30, self.api.opened)

    def test_invalid_pid_receipt_fails_closed(self):
        for value in ("", "0\n", "30 " , "9" * 33):
            with self.subTest(value=value):
                self.receipt.write_text(value)
                with self.assertRaisesRegex(AssertionError, "invalid native PID"):
                    self.registry.capture()
        self.assertNotIn(30, self.api.opened)

    def test_cleanup_uses_retained_object_not_reused_pid(self):
        self.chain()
        self.api.objects["native-h"].update(alive=False, exitCode=0)
        self.api.add("external-new-native", 30, 999, 1100, self.paths["native"])
        actions = self.registry.terminate_known()
        self.assertEqual(self.api.terminated, ["launcher-h", "bridge-h"])
        self.assertTrue(self.api.objects["external-new-native"]["alive"])
        self.assertEqual({a["role"] for a in actions}, {"launcher", "bridge"})

    def test_cleanup_refuses_changed_handle_identity(self):
        self.chain()
        self.api.objects["native-h"]["identity"]["creationTime100ns"] += 1
        actions = self.registry.terminate_known()
        self.assertNotIn("native-h", self.api.terminated)
        self.assertTrue(any(a.get("refusedOrFailed") for a in actions))

    def peer(self):
        peer = harness.Peer.__new__(harness.Peer)  # No Popen.
        peer.directory, peer.pids = self.directory, self.registry
        peer.process = SimpleNamespace(pid=10, stdin=io.BytesIO(), returncode=0,
                                       wait=lambda timeout: 0)
        peer.sent, peer.readers, peer.reader_errors = io.BytesIO(), [], []
        peer.counts, peer.reader_counts = {}, {}
        return peer

    def test_peer_normal_eof_requires_zero_and_no_known_survivors(self):
        self.chain()
        peer = self.peer()
        def eof():
            for obj in self.api.objects.values():
                obj.update(alive=False, exitCode=0)
        peer.process.stdin = SimpleNamespace(close=eof)
        result = peer.close()
        self.assertFalse(result["forcedKills"])
        self.assertEqual(result["bridgeExitCode"], 0)
        self.assertFalse(self.api.terminated)

    def test_peer_manual_cleanup_always_failure(self):
        self.chain()
        peer = self.peer()
        with patch.object(harness.time, "monotonic", side_effect=[0, 20]):
            with self.assertRaisesRegex(AssertionError, "unclean bridge shutdown"):
                peer.close()
        saved = json.loads((self.directory / "cleanup.json").read_text())
        self.assertEqual(len(saved["forcedKills"]), 3)
        self.assertEqual(self.api.terminated, ["native-h", "launcher-h", "bridge-h"])

    def native_audit_fixture(self, code, receipt):
        self.chain()
        before = self.registry.status()
        for obj in self.api.objects.values():
            obj.update(alive=False, exitCode=0)
        self.api.objects["native-h"]["exitCode"] = code
        harness.save(self.directory / "cleanup.json", dict(beforeEOF=before,
            after=self.registry.status(), bridgeExitCode=0, forcedKills=[]))
        logs = self.receipt.parent
        (logs / "native-input.jsonl").write_text('{"type":"get_state"}\n')
        (logs / "native-output.jsonl").write_text('{"type":"ready"}\n')
        if receipt is not None:
            (logs / "native-exit.json").write_bytes(receipt)
        return logs

    def test_optional_receipt_missing_or_incomplete_uses_actual_native_handle(self):
        for code in (0, 1):
            for receipt, state in ((None, "missing"), (b'{"code":', "incomplete")):
                with self.subTest(code=code, receipt=receipt):
                    self.native_audit_fixture(code, receipt)
                    result = harness.audit_native(self.directory)
                    self.assertEqual(result["nativeProcessExit"]["exitCode"], code)
                    self.assertEqual(result["launcherReceiptState"], state)
                    self.assertFalse(result["gracefulNativeShutdownQualified"])
                    (self.receipt.parent / "native-exit.json").unlink(missing_ok=True)
                    for obj in self.api.objects.values():
                        obj.update(alive=True, exitCode=259)

    def test_optional_complete_receipt_must_agree_with_handle(self):
        self.native_audit_fixture(1, b'{"code":0}')
        with self.assertRaisesRegex(AssertionError, "disagrees"):
            harness.audit_native(self.directory)

    def test_native_audit_still_rejects_execution(self):
        logs = self.native_audit_fixture(0, None)
        (logs / "native-output.jsonl").write_text('{"type":"ready"}\n{"type":"agent_start"}\n')
        with self.assertRaisesRegex(AssertionError, "execution or non-read-only"):
            harness.audit_native(self.directory)

    def test_native_audit_rejects_manual_cleanup(self):
        self.native_audit_fixture(1, None)
        path = self.directory / "cleanup.json"
        cleanup = json.loads(path.read_text())
        cleanup["forcedKills"] = [{"role": "native", "method": "mock retained handle"}]
        harness.save(path, cleanup)
        with self.assertRaisesRegex(AssertionError, "normal bridge EOF"):
            harness.audit_native(self.directory)

    def test_explicit_frozen_sha_binding(self):
        expected = {role: harness.sha(path) for role, path in self.paths.items()}
        expected["bridge"] = "0" * 64
        with self.assertRaisesRegex(AssertionError, "SHA changed"):
            harness.Processes(SimpleNamespace(pid=10, _handle="bridge-h"),
                self.paths["bridge"], self.paths["launcher"], self.paths["native"],
                self.receipt, api=self.api, expected_hashes=expected)
        self.assertFalse(self.api.terminated)

    def test_static_no_pid_tree_termination_or_history_discovery(self):
        source = Path(harness.__file__).read_text()
        tree = ast.parse(source)
        processes = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "Processes")
        registry_source = ast.get_source_segment(source, processes)
        self.assertNotIn("rglob", registry_source)
        self.assertNotIn("taskkill", source.casefold())
        self.assertNotIn('"/T"', source)
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                self.assertNotIn(node.func.attr, {"kill", "terminate"} if
                    isinstance(node.func.value, ast.Name) and node.func.value.id == "subprocess" else set())


if __name__ == "__main__":
    unittest.main(verbosity=2)
