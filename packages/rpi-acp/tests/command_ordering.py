"""Opening-response/commands ordering against a real bridge + offline native peer.

The new-session client model intentionally drops notifications for unknown IDs,
matching pinned Zed 1.25.3. Replay on load still precedes its response. This is
not GUI acceptance and does not execute native compact.
"""
import argparse
import json
from pathlib import Path
import queue
import tempfile
import time
import unittest

from protocol_regression import Bridge


class CommandOrdering(unittest.TestCase):
    bridge = None
    rpc = None

    def receive_open(self, bridge, method, params, commands):
        bridge.next_id += 1
        rid = bridge.next_id
        bridge.send({'jsonrpc': '2.0', 'id': rid, 'method': method, 'params': params})
        frames = []
        response = None
        deadline = time.monotonic() + 20
        registered = False
        seen_commands = []
        while response is None or (commands and not seen_commands):
            frame = bridge.output.get(timeout=max(.01, deadline - time.monotonic()))
            frames.append(frame)
            if frame.get('id') == rid:
                self.assertIsNone(response, 'duplicate opening response')
                self.assertIn('result', frame, frame)
                response = frame
                registered = True
            if frame.get('params', {}).get('update', {}).get('sessionUpdate') == 'available_commands_update':
                # Fail on the actual old ordering, rather than accepting an
                # announcement that a client silently discarded.
                self.assertTrue(registered, f'{method}: commands preceded registration response')
                seen_commands.append(frame)
            self.assertLess(time.monotonic(), deadline)
        if not commands:
            # A response to a subsequent request is a transport barrier, not a
            # sleep-based guess that there will never be a notification.
            _, after = bridge.request('ordering-test-unknown-method', {})
            self.assertFalse(any(f.get('params', {}).get('update', {}).get('sessionUpdate')
                == 'available_commands_update' for f in frames + after))
        else:
            _, after = bridge.request('ordering-test-unknown-method', {})
            seen_commands += [f for f in after if f.get('params', {}).get('update', {}).get('sessionUpdate')
                == 'available_commands_update']
            self.assertEqual(len(seen_commands), 1)
            self.assertEqual([c['name'] for c in seen_commands[0]['params']['update']['availableCommands']], ['compact'])
        return response['result'], frames

    def run_case(self, version, compact):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'native.jsonl'
            env = {'RPI_HISTORY_LOG': log}
            if compact:
                env['RPI_HISTORY_COMPACT'] = '1'
            bridge = Bridge(self.bridge, self.rpc, root / 'sessions', root, extra_env=env)
            try:
                init = {'protocolVersion': 1, 'clientCapabilities': {}} if version == 1 else {
                    'protocolVersion': 2, 'info': {'name': 'ordering', 'version': '1'}, 'capabilities': {}}
                response, _ = bridge.request('initialize', init)
                self.assertIn('result', response)
                result, _ = self.receive_open(bridge, 'session/new', {'cwd': str(root), 'mcpServers': []},
                    commands=compact and version == 1)
                sid = result['sessionId']
                methods = ['session/load', 'session/resume'] if version == 1 else ['session/resume']
                for method in methods:
                    params = {'sessionId': sid, 'cwd': str(root)}
                    if version == 2:
                        params['replayFrom'] = {'type': 'start'}
                    _, frames = self.receive_open(bridge, method, params, commands=compact and version == 1)
                    replay = [i for i, f in enumerate(frames) if f.get('params', {}).get('update', {}).get('sessionUpdate') == 'user_message_chunk']
                    response_index = next(i for i, f in enumerate(frames) if 'result' in f)
                    if method == 'session/load' or version == 2:
                        self.assertEqual(len(replay), 513)
                        self.assertLess(max(replay), response_index, 'load replay must remain pre-response')
                    else:
                        self.assertEqual(replay, [])
                native = [json.loads(line)['type'] for line in log.read_text(encoding='utf8').splitlines()]
                self.assertNotIn('compact', native, 'ordering test must not pretend to compact')
                self.assertNotIn('get_commands', native, 'no plugins should be needed')
            finally:
                bridge.close()
                bridge.process.stdout.close()
                bridge.process.stderr.close()
            self.assertEqual(bridge.process.returncode, 0)

    def test_v1_commands_follow_new_load_resume_response(self):
        self.run_case(1, True)

    def test_old_host_no_commands(self):
        self.run_case(1, False)

    def test_v2_does_not_advertise_local_shortcut(self):
        self.run_case(2, True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--rpc', type=Path, required=True)
    args, rest = parser.parse_known_args()
    CommandOrdering.bridge = args.bridge.resolve()
    CommandOrdering.rpc = args.rpc.resolve()
    unittest.main(argv=[__file__] + rest)
