"""Real bridge transport fixture for bounded load and private history paging.
Build --bin rpi-acp-bridge --example history_rpc, then pass both paths.
"""
import argparse
import json
from pathlib import Path
import tempfile
import unittest
from protocol_regression import Bridge


class HistoryProtocol(unittest.TestCase):
    bridge = None
    rpc = None

    def start(self, root, version=1, **env):
        bridge = Bridge(self.bridge, self.rpc, root / 'sessions', root, extra_env=env)
        params = {'protocolVersion': version, 'clientCapabilities': {}} if version == 1 else {
            'protocolVersion': 2, 'info': {'name': 'history-fixture', 'version': '1'}, 'capabilities': {}}
        result, _ = bridge.request('initialize', params)
        self.assertEqual(result['result']['protocolVersion'], version, result)
        result, _ = bridge.request('session/new', {'cwd': str(root), 'mcpServers': []})
        self.assertIn('result', result, result)
        return bridge, result['result']['sessionId']

    @staticmethod
    def text_updates(messages):
        return [m['params']['update']['content']['text'] for m in messages
                if m.get('method') == 'session/update' and
                m['params']['update'].get('sessionUpdate') == 'user_message_chunk']

    def test_old_host_does_not_receive_optional_modes_or_unexecutable_commands(self):
        for version in (1, 2):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                log = root / 'native.jsonl'
                bridge, sid = self.start(root, version=version, RPI_HISTORY_LOG=log,
                    RPI_HISTORY_DISCOVERY_ONLY='1', RPI_EXTENSIONS_DIR=root)
                try:
                    method = 'session/load' if version == 1 else 'session/resume'
                    result, messages = bridge.request(method, {'sessionId': sid, 'cwd': str(root)})
                    self.assertIn('result', result, result)
                    self.assertNotIn('modes', result['result'])
                    self.assertFalse(any(option.get('id', option.get('configId')) == 'mode'
                        for option in result['result']['configOptions']))
                    self.assertFalse(any(m.get('params', {}).get('update', {}).get('sessionUpdate')
                        == 'available_commands_update' for m in messages))
                    result, _ = bridge.request('session/set_config_option', {
                        'sessionId': sid, 'configId': 'mode', 'value': 'plan'})
                    self.assertEqual(result['error']['code'], -32602, result)
                    if version == 1:
                        result, _ = bridge.request('session/set_mode', {'sessionId': sid, 'modeId': 'plan'})
                        self.assertEqual(result['error']['code'], -32602, result)
                    requests = [json.loads(line)['type'] for line in log.read_text().splitlines()]
                    for unsupported in ('get_modes', 'set_mode', 'get_commands'):
                        self.assertNotIn(unsupported, requests)
                finally:
                    bridge.close()

    def test_v1_full_load_is_oldest_first_and_native_requests_are_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'native.jsonl'
            bridge, sid = self.start(root, RPI_HISTORY_LOG=log)
            try:
                result, messages = bridge.request('session/load', {'sessionId': sid, 'cwd': str(root)})
                self.assertIn('result', result, result)
                self.assertEqual(self.text_updates(messages), [f'history-{n}' for n in range(513)])
                requests = [json.loads(line) for line in log.read_text().splitlines()]
                pages = [r for r in requests if r['type'] == 'get_messages']
                self.assertEqual([r['limit'] for r in pages], [128] * 5 + [1])
                self.assertEqual([r['beforeId'] for r in pages], [None, '385', '257', '129', '1', None])
            finally:
                bridge.close()

    def test_private_pages_continue_without_omission_and_reject_bad_cursors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bridge, sid = self.start(root)
            try:
                cursor = None
                observed = []
                while True:
                    result, messages = bridge.request('_rpi/history', {'sessionId': sid, 'limit': 31, 'cursor': cursor})
                    self.assertIn('result', result, result)
                    self.assertEqual(messages, [])
                    page = result['result']
                    updates = page['updates']
                    self.assertLessEqual(len(updates), 31)
                    observed[0:0] = [u['content']['text'] for u in updates]
                    cursor = page['nextCursor']
                    if cursor is None:
                        self.assertFalse(page['hasMore'])
                        break
                    self.assertTrue(page['hasMore'])
                    # A cursor must not cross session boundaries.
                    bad, _ = bridge.request('_rpi/history', {'sessionId': 'another', 'cursor': cursor})
                    self.assertEqual(bad['error']['code'], -32602)
                self.assertEqual(observed, [f'history-{n}' for n in range(513)])
                for bad_params in [{'cursor': 'not-json'}, {'limit': 0}, {'limit': 129}, {'cursor': 7}]:
                    result, _ = bridge.request('_rpi/history', {'sessionId': sid, **bad_params})
                    self.assertEqual(result['error']['code'], -32602, result)
            finally:
                bridge.close()

    def test_v2_start_replays_null_does_not_and_unknown_rejects(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bridge, sid = self.start(root, version=2)
            try:
                for cursor in [None, {'type': 'start'}]:
                    result, messages = bridge.request('session/resume', {'sessionId': sid, 'cwd': str(root), 'replayFrom': cursor})
                    self.assertIn('result', result, result)
                    self.assertEqual(self.text_updates(messages), [] if cursor is None else [f'history-{n}' for n in range(513)])
                    self.assertEqual(messages[-1]['params']['update'], {'sessionUpdate': 'state_update', 'state': 'idle'})
                for cursor in ['start', {}, {'type': 'future'}, {'type': '_unsupported'}]:
                    result, messages = bridge.request('session/resume', {'sessionId': sid, 'cwd': str(root), 'replayFrom': cursor})
                    self.assertEqual(result['error']['code'], -32602)
                    self.assertEqual(messages, [])
            finally:
                bridge.close()

    def test_v2_load_is_undefined_and_active_no_replay_resume_reports_running(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'native.jsonl'
            bridge, sid = self.start(root, version=2, RPI_HISTORY_LOG=log)
            try:
                result, messages = bridge.request('session/load', {'sessionId': sid, 'cwd': str(root)})
                self.assertEqual(result['error']['code'], -32601, result)
                self.assertEqual(messages, [])
                bridge.send({'jsonrpc': '2.0', 'id': 901, 'method': 'session/prompt',
                             'params': {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'hold'}]}})
                import time
                deadline = time.monotonic() + 5
                while not any(json.loads(line)['type'] == 'prompt' for line in log.read_text().splitlines()):
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                # v2 replies at canonical insertion, not at settlement. The
                # fixture must provide that insertion before testing resume.
                inserted, _ = bridge.wait_response(901)
                self.assertEqual(inserted.get('result', {}).get('messageId'), 'history:held-user', inserted)
                result, messages = bridge.request('session/resume', {'sessionId': sid, 'cwd': str(root), 'replayFrom': None})
                self.assertIn('result', result, result)
                self.assertEqual(messages[-1]['params']['update'], {'sessionUpdate': 'state_update', 'state': 'running'})
                self.assertFalse(any(json.loads(line)['type'] == 'get_messages' for line in log.read_text().splitlines()))
                bridge.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': sid}})
                deadline = time.monotonic() + 5
                while True:
                    update = bridge.output.get(timeout=max(.01, deadline - time.monotonic()))
                    self.assertNotIn('id', update, 'v2 must not answer prompt twice')
                    state = update.get('params', {}).get('update', {})
                    if state.get('sessionUpdate') == 'state_update' and state.get('state') == 'idle':
                        self.assertEqual(state.get('stopReason'), 'cancelled', update)
                        break
            finally:
                bridge.close()

    def test_reopened_child_is_cleaned_up_after_invalid_history(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pids = root / 'pids.log'
            bridge, sid = self.start(root, RPI_HISTORY_BAD_PAGE=1, RPI_HISTORY_PID_LOG=pids)
            try:
                result, _ = bridge.request('session/close', {'sessionId': sid})
                self.assertIn('result', result, result)
                # A child exiting before handle acquisition also proves cleanup.
                bridge.send({'jsonrpc': '2.0', 'id': 902, 'method': 'session/load',
                             'params': {'sessionId': sid, 'cwd': str(root)}})
                pid = bridge.wait_pid(pids, count=2)[-1]
                # An already-exited child also proves cleanup; on Windows it may
                # no longer be openable once the parent has reaped it.
                result, _ = bridge.wait_response(902)
                self.assertIn('error', result, result)
                import os
                if os.name == 'nt':
                    kernel32 = Bridge._kernel32()
                    handle = kernel32.OpenProcess(0x00100000, False, pid)
                    if handle:
                        try:
                            self.assertEqual(kernel32.WaitForSingleObject(handle, 5000), 0)
                        finally:
                            kernel32.CloseHandle(handle)
                else:
                    Bridge.wait_pid_exit(pid)
            finally:
                bridge.close()

    def test_close_and_delete_serialize_with_reopened_load(self):
        import time
        for method in ['session/close', 'session/delete']:
            with self.subTest(method=method), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                log = root / 'native.jsonl'
                bridge, sid = self.start(root, RPI_HISTORY_DELAY=40, RPI_HISTORY_LOG=log)
                try:
                    result, _ = bridge.request('session/close', {'sessionId': sid})
                    self.assertIn('result', result, result)
                    bridge.send({'jsonrpc': '2.0', 'id': 903, 'method': 'session/load',
                                 'params': {'sessionId': sid, 'cwd': str(root)}})
                    deadline = time.monotonic() + 5
                    while not any(json.loads(line)['type'] == 'get_messages' for line in log.read_text().splitlines()):
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(.01)
                    bridge.send({'jsonrpc': '2.0', 'id': 904, 'method': method, 'params': {'sessionId': sid}})
                    replies = {}
                    while len(replies) < 2:
                        message = bridge.output.get(timeout=10)
                        if message.get('id') in [903, 904]:
                            replies[message['id']] = message
                    self.assertIn('result', replies[903], replies)
                    self.assertIn('result', replies[904], replies)
                    result, _ = bridge.request('_rpi/history', {'sessionId': sid})
                    self.assertEqual(result['error']['code'], -32602, result)
                    if method == 'session/delete':
                        self.assertFalse((root / 'sessions' / sid).exists())
                finally:
                    bridge.close()

    def test_invalid_page_or_branch_change_never_emits_partial_load(self):
        for env in [{'RPI_HISTORY_BAD_PAGE': 1}, {'RPI_HISTORY_BRANCH_CHANGE': 1}]:
            with self.subTest(env=env), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                bridge, sid = self.start(root, **env)
                try:
                    result, messages = bridge.request('session/load', {'sessionId': sid, 'cwd': str(root)})
                    self.assertIn('error', result, result)
                    self.assertEqual(self.text_updates(messages), [])
                finally:
                    bridge.close()

    def test_cancel_stalled_replay_releases_lease_for_next_resume(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'native.jsonl'
            bridge, sid = self.start(root, RPI_HISTORY_DELAY=300, RPI_HISTORY_LOG=log)
            try:
                bridge.send({'jsonrpc': '2.0', 'id': 900, 'method': 'session/load', 'params': {'sessionId': sid, 'cwd': str(root)}})
                import time
                deadline = time.monotonic() + 5
                while not any(json.loads(line)['type'] == 'get_messages' for line in log.read_text().splitlines()):
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                bridge.send({'jsonrpc': '2.0', 'method': '$/cancel_request', 'params': {'requestId': 900}})
                result = bridge.output.get(timeout=5)
                self.assertEqual(result.get('id'), 900, result)
                self.assertEqual(result['error']['code'], -32800, result)
                result, _ = bridge.request('session/resume', {'sessionId': sid, 'cwd': str(root)})
                self.assertIn('result', result, result)
            finally:
                bridge.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--rpc', type=Path, required=True)
    args, rest = parser.parse_known_args()
    HistoryProtocol.bridge = args.bridge.resolve()
    HistoryProtocol.rpc = args.rpc.resolve()
    unittest.main(argv=['history_protocol.py', *rest])
