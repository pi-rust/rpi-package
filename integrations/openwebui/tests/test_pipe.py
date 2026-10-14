import asyncio
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[3] / "packages/rpi-webui/openwebui/rpi_acp_pipe.py"
spec = importlib.util.spec_from_file_location("rpi_acp_pipe", SOURCE)
pipe_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pipe_module)


class ChoiceTests(unittest.TestCase):
    def test_multiselect_numbers_and_json_labels(self):
        choose = pipe_module.select_values
        self.assertEqual(choose("1，2", ["Rust", "Python"], True), ["Rust", "Python"])
        self.assertEqual(choose('["a,b", "中文"]', ["a,b", "中文"], True), ["a,b", "中文"])
        self.assertEqual(choose("text:3", ["Rust"], freeform=True), ["3"])
        for raw in ["1,1", "3", '[true]', '[]']:
            with self.assertRaises(ValueError):
                choose(raw, ["Rust", "Python"], True)
        with self.assertRaises(ValueError):
            choose("1,2", ["Rust", "Python"])

    def test_worker_lease_and_owner_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            store = pipe_module.StateStore(directory)
            self.assertEqual(store.acquire("a", "owner", 10), (None, None))
            with self.assertRaises(RuntimeError):
                pipe_module.StateStore(directory).acquire("a", "other", 10)
            store.release("a", "wrong", "bad", "bad")
            with self.assertRaises(RuntimeError):
                store.acquire("a", "other", 10)
            store.release("a", "owner", "session", "history")
            self.assertEqual(store.acquire("a", "other", 10), ("session", "history"))
            store.release("a", "other")
            self.assertEqual(store.acquire("a", "third", 10), (None, None))


class PipeTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.peer = self.root / "peer"
        self.pipe = pipe_module.Pipe()
        self.pipe.valves.ACP_COMMAND = sys.executable
        self.pipe.valves.ACP_ARGS = json.dumps([str(Path(__file__).with_name("fake_acp.py")), str(self.peer)])
        self.pipe.valves.WORKSPACE = str(self.root)
        self.pipe.valves.STATE_DIR = str(self.root / "state")
        self.pipe.valves.TURN_TIMEOUT = 5
        self.statuses = []
        self.dialogs = []
        self.answers = []

    async def emit(self, event):
        self.statuses.append(event)

    async def call(self, event):
        self.dialogs.append(event)
        return self.answers.pop(0)

    async def run_pipe(self, messages, user="u", chat="c", event_call=True):
        async def consume():
            return "".join([chunk async for chunk in self.pipe.pipe(
                {"messages": messages}, __user__={"id":user}, __chat_id__=chat,
                __event_emitter__=self.emit, __event_call__=self.call if event_call else None,
            )])
        return await asyncio.wait_for(consume(), 8)

    def log(self):
        return [json.loads(line) for line in (self.peer / "messages.jsonl").read_text(encoding="utf-8").splitlines()]

    async def test_stream_finishes_without_a_notification_after_prompt_response(self):
        self.assertEqual(await self.run_pipe([{"role":"user", "content":"Hello"}]), "完成🌊")
        self.assertTrue(self.statuses[-1]["data"]["done"])
        self.assertEqual(self.log()[0]["params"]["clientCapabilities"]["_meta"]["pi-rust/questions"], 1)

    async def test_questions_comments_and_explicit_permission_rejection(self):
        self.answers = ["1,2", "一起用", "2"]
        text = await self.run_pipe([{"role":"user", "content":"dialogs"}])
        self.assertIn("完成🌊", text)
        self.assertIn("&lt;script&gt;danger&lt;/script&gt;", text)
        self.assertNotIn("delta.tool_calls", text)
        answers = {m["id"]:m["result"] for m in self.log() if "result" in m}
        self.assertEqual(answers[100], {"answer":{"values":["Rust","Python"], "text":"一起用"}})
        self.assertEqual(answers[101], {"outcome":{"outcome":"selected", "optionId":"reject"}})
        self.assertEqual(len(self.dialogs), 3)

    async def test_absent_browser_cancels_questions_and_permissions(self):
        await self.run_pipe([{"role":"user", "content":"dialogs"}], event_call=False)
        answers = {m["id"]:m["result"] for m in self.log() if "result" in m}
        self.assertEqual(answers[100], {"cancelled":True})
        self.assertEqual(answers[101], {"outcome":{"outcome":"cancelled"}})

    async def test_bypass_skips_gate_and_preserves_business_questions(self):
        self.pipe.valves.PERMISSION_MODE = "bypass"
        self.answers = ["1,2", "comment"]
        with patch.dict(os.environ, {"RPI_ACP_NO_PERMISSION_GATE":"0", "RPI_PERMISSION_GATE":"1"}):
            self.assertEqual(await self.run_pipe([{"role":"user", "content":"dialogs"}]), "完成🌊")
        gate = json.loads((self.peer / "gate-env.json").read_text())
        self.assertEqual(gate, {"RPI_ACP_NO_PERMISSION_GATE":"1", "RPI_PERMISSION_GATE":"0"})
        self.assertEqual(len(self.dialogs), 2)  # question and its optional comment
        self.assertEqual([m["result"] for m in self.log() if m.get("id") == 100],
                         [{"answer":{"values":["Rust","Python"],"text":"comment"}}])
        self.assertFalse(any(m.get("id") == 101 for m in self.log()))
        # Switching back must enable approvals even if the service environment bypasses them.
        self.pipe.valves.PERMISSION_MODE = "ask"
        self.answers = ["1", "", "2"]
        with patch.dict(os.environ, {"RPI_ACP_NO_PERMISSION_GATE":"1", "RPI_PERMISSION_GATE":"0"}):
            await self.run_pipe([{"role":"user", "content":"dialogs again"}], chat="ask-chat")
        self.assertEqual(json.loads((self.peer / "gate-env.json").read_text()),
                         {"RPI_ACP_NO_PERMISSION_GATE":"0", "RPI_PERMISSION_GATE":"1"})
        answers = [m["result"] for m in self.log() if m.get("id") == 101]
        self.assertEqual(answers[-1], {"outcome":{"outcome":"selected", "optionId":"reject"}})

    async def test_native_cancel_false_retires_dialog_without_reopening_it(self):
        self.answers = [False, False]
        await self.run_pipe([{"role":"user", "content":"dialogs"}])
        answers = {m["id"]:m["result"] for m in self.log() if "result" in m}
        self.assertEqual(answers[100], {"cancelled":True})
        self.assertEqual(answers[101], {"outcome":{"outcome":"cancelled"}})
        self.assertEqual(len(self.dialogs), 2)
        self.assertEqual(self.dialogs[-1]["data"]["type"], "select")
        self.assertEqual(self.dialogs[-1]["data"]["options"], [{"label":"允许一次","value":"允许一次"},{"label":"拒绝","value":"拒绝"}])

    async def test_session_resumes_and_other_users_are_isolated(self):
        first = [{"role":"user", "content":"first"}]
        answer = await self.run_pipe(first)
        second = first + [{"role":"assistant", "content":answer}, {"role":"user", "content":"second"}]
        await self.run_pipe(second)
        methods = [m.get("method") for m in self.log()]
        self.assertEqual(methods.count("session/resume"), 1)
        self.assertEqual([m for m in self.log() if m.get("method") == "session/prompt"][-1]["params"]["prompt"][0]["text"], "second")
        await self.run_pipe(second, user="other")
        self.assertEqual(sum(m.get("method") == "session/new" for m in self.log()), 2)

    async def test_edited_history_creates_new_session_with_full_context(self):
        await self.run_pipe([{"role":"user", "content":"first"}])
        await self.run_pipe([{"role":"user", "content":"edited"}, {"role":"assistant", "content":"old"}, {"role":"user", "content":"second"}])
        self.assertEqual(sum(m.get("method") == "session/new" for m in self.log()), 2)
        self.assertIn("edited", [m for m in self.log() if m.get("method") == "session/prompt"][-1]["params"]["prompt"][0]["text"])

    async def test_closed_question_never_sends_a_late_answer(self):
        async def blocked(event):
            await asyncio.sleep(10)
            return "late"
        self.pipe.valves.DIALOG_TIMEOUT = 1
        self.call = blocked
        self.assertEqual(await self.run_pipe([{"role":"user", "content":"closed dialog"}]), "完成🌊")
        self.assertFalse(any("result" in m for m in self.log()))

    async def test_unanswered_dialogs_time_out_as_cancellation(self):
        async def blocked(event):
            await asyncio.sleep(10)
        self.call = blocked
        self.pipe.valves.DIALOG_TIMEOUT = 1
        await self.run_pipe([{"role":"user", "content":"dialogs"}])
        answers = {m["id"]:m["result"] for m in self.log() if "result" in m}
        self.assertEqual(answers[100], {"cancelled":True})
        self.assertEqual(answers[101], {"outcome":{"outcome":"cancelled"}})

    async def test_stopping_the_stream_cancels_agent_and_releases_chat_lease(self):
        entered = asyncio.Event()
        async def blocked(event):
            entered.set()
            await asyncio.sleep(10)
        self.call = blocked
        task = asyncio.create_task(self.run_pipe([{"role":"user", "content":"dialogs"}]))
        await asyncio.wait_for(entered.wait(), 2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(any(m.get("method") == "session/cancel" for m in self.log()))
        self.assertEqual(await self.run_pipe([{"role":"user", "content":"retry"}]), "完成🌊")

    async def test_transport_disconnect_fails_prompt_and_releases_lease(self):
        with self.assertRaisesRegex(RuntimeError, "disconnected"):
            await self.run_pipe([{"role":"user", "content":"disconnect"}])
        self.assertEqual(await self.run_pipe([{"role":"user", "content":"retry"}]), "完成🌊")

    async def test_title_generation_does_not_spawn_an_agent(self):
        results = [chunk async for chunk in self.pipe.pipe({}, __task__="title_generation")]
        self.assertEqual(results, [])
        self.assertFalse(self.peer.exists())

    async def test_invalid_choice_retries_and_timeout_cancels(self):
        client = pipe_module.AcpClient("", [], str(self.root), self.call)
        self.answers = ["3", "1,2", "1"]
        result = await client.choose({"question":"选择", "options":["Rust", "Python"]}, 1)
        self.assertEqual(result, {"answer":{"values":["Rust"]}})
        self.assertEqual(len(self.dialogs), 3)
        async def blocked(event):
            await asyncio.sleep(10)
        client.event_call = blocked
        with self.assertRaises(asyncio.TimeoutError):
            await client.choose({"question":"选择", "options":["Rust"]}, .01)

    @unittest.skipUnless(os.environ.get("RPI_TEST_ACP_BRIDGE") and os.environ.get("RPI_TEST_RPC_PEER"),
                         "Set RPI_TEST_ACP_BRIDGE and RPI_TEST_RPC_PEER for the compiled bridge round trip")
    async def test_compiled_rust_bridge_returns_original_answers_to_rpc_peer(self):
        self.pipe.valves.ACP_COMMAND = os.environ["RPI_TEST_ACP_BRIDGE"]
        self.pipe.valves.ACP_ARGS = "[]"
        self.answers = ["1,2", "一起用", "2"]
        rpc_log = self.root / "rpc.jsonl"
        with patch.dict(os.environ, {
            "RPI_BIN":os.environ["RPI_TEST_RPC_PEER"],
            "RPI_ACP_SESSION_DIR":str(self.root / "native-sessions"),
            "RPI_TEST_RPC_LOG":str(rpc_log),
        }):
            self.assertEqual(await self.run_pipe([{"role":"user","content":"语言"}]), "完成🌊")
        responses = [json.loads(line) for line in rpc_log.read_text(encoding="utf-8").splitlines()]
        answers = {m["requestId"]:m for m in responses if m["type"] == "ui_dialog_response"}
        self.assertEqual(answers["question"]["answer"], {"values":["Rust","Python"],"text":"一起用"})
        self.assertEqual(answers["permission"]["answer"], {"values":["拒绝"]})


if __name__ == "__main__":
    unittest.main()
