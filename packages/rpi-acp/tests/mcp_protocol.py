"""Deterministic ACP v1/v2 -> native external tools -> real stdio MCP.

Build rpi-acp-bridge and --example fake_mcp_rpc, then pass --bridge/--rpc.
The same file supplies the isolated MCP subprocess; no model or install needed.
"""
import argparse
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from protocol_regression import Bridge

SECRET = 'mcp-regression-secret-DO-NOT-PERSIST'


def serve():
    with open(os.environ['MCP_PID_LOG'], 'a', encoding='utf-8') as log:
        log.write(str(os.getpid()) + '\n')
    for line in sys.stdin:
        request = json.loads(line)
        method = request.get('method')
        if 'id' not in request:
            continue
        reply = {'jsonrpc': '2.0', 'id': request['id']}
        if method == 'initialize':
            reply['result'] = {'protocolVersion': request['params']['protocolVersion'],
                               'capabilities': {'tools': {}},
                               'serverInfo': {'name': 'deterministic', 'version': '1'}}
        elif method == 'tools/list':
            reply['result'] = {'tools': [{'name': 'echo / punctuation',
                'description': 'Fixture', 'inputSchema': {'type': 'object'}}]}
        elif method == 'tools/call':
            assert request['params']['name'] == 'echo / punctuation'
            mode = request['params']['arguments']['mode']
            if mode == 'rpc-error':
                reply['error'] = {'code': -32603, 'message': SECRET}
            else:
                reply['result'] = {'content': [{'type': 'text', 'text': 'fixture:' + mode}],
                    'isError': mode == 'tool-error',
                    'structuredContent': {'envReceived': os.environ.get('MCP_SECRET') == SECRET}}
        else:
            reply['error'] = {'code': -32601, 'message': 'unsupported'}
        print(json.dumps(reply), flush=True)


class McpProtocol(unittest.TestCase):
    def test_real_stdio_mapping_lifecycle(self):
        for version in (1, 2):
            with self.subTest(version=version), tempfile.TemporaryDirectory(prefix='acp-mcp-') as tmp:
                root = Path(tmp)
                store = root / 'sessions'
                pid_log = root / 'mcp-pids'
                native_log = root / 'native.jsonl'
                bridge = Bridge(self.bridge, self.rpc, store, root,
                    extra_env={'RPI_TEST_MCP_NATIVE_LOG': native_log})
                handles = []
                frames = []

                def request(method, params):
                    reply, updates = bridge.request(method, params)
                    frames.extend(updates + [reply])
                    self.assertIn('result', reply, reply)
                    if version == 2 and method == 'session/prompt':
                        self.assertTrue(reply['result'].get('messageId'), reply)
                        while not any(m.get('method') == 'session/update' and
                                      m.get('params', {}).get('update', {}).get('state') == 'idle'
                                      for m in updates):
                            frame = bridge.output.get(timeout=10)
                            frames.append(frame)
                            updates.append(frame)
                    return reply['result']

                def server():
                    value = {'name': 'Human tools / punctuation', 'command': str(Path(sys.executable).resolve()),
                        'args': [str(Path(__file__).resolve()), '--serve'], 'env': [
                            {'name': 'MCP_PID_LOG', 'value': str(pid_log)},
                            {'name': 'MCP_SECRET', 'value': SECRET}], '_meta': {'ignored': True}}
                    if version == 2:
                        value['type'] = 'stdio'
                    return value

                try:
                    init = {'protocolVersion': version}
                    if version == 2:
                        init.update(info={'name': 'mcp-test', 'version': '1'}, capabilities={})
                    request('initialize', init)
                    # Invalid maps fail before spawning any MCP process and never echo secrets.
                    for invalid in [dict(server(), command=SECRET), dict(server(), type='sse'),
                                    dict(server(), env=[{'name': 'BAD=NAME', 'value': SECRET}])]:
                        reply, updates = bridge.request('session/new', {'cwd': str(root), 'mcpServers': [invalid]})
                        frames.extend(updates + [reply])
                        self.assertEqual(reply.get('error', {}).get('code'), -32602, reply)
                    self.assertFalse(pid_log.exists())
                    sid = None
                    lifecycle = ('session/new', 'session/load', 'session/resume') if version == 1 else ('session/new', 'session/resume', 'session/resume')
                    for index, method in enumerate(lifecycle):
                        params = {'cwd': str(root), 'mcpServers': [server()]}
                        if sid:
                            params['sessionId'] = sid
                        result = request(method, params)
                        sid = result.get('sessionId', sid)
                        pids = bridge.wait_pid(pid_log, count=index + 1)
                        handle = Bridge.open_pid_handle(pids[-1])
                        handles.append(handle)
                        for mode in ('success', 'tool-error', 'rpc-error'):
                            result = request('session/prompt', {'sessionId': sid,
                                'prompt': [{'type': 'text', 'text': mode}]})
                            if version == 1:
                                self.assertEqual(result.get('stopReason'), 'end_turn', result)
                            else:
                                self.assertTrue(result.get('messageId'), result)
                            records = [json.loads(line) for line in native_log.read_text().splitlines()]
                            response = [r for r in records if r.get('type') == 'external_tool_response'][-1]
                            if mode == 'rpc-error':
                                self.assertIn('error', response, response)
                                self.assertNotIn(SECRET, json.dumps(response))
                            else:
                                self.assertEqual(response['result']['content'][0]['text'], 'fixture:' + mode)
                                self.assertEqual(response['result']['isError'], mode == 'tool-error')
                                self.assertTrue(response['result']['structuredContent']['envReceived'])
                        if index < 2:
                            request('session/close', {'sessionId': sid})
                            Bridge.wait_handle_exit(handles.pop())
                    # EOF must also reap the final session-owned MCP subprocess.
                    bridge.close()
                    Bridge.wait_handle_exit(handles.pop())
                    self.assertEqual(len(bridge.wait_pid(pid_log, count=3)), 3)
                    records = [json.loads(line) for line in native_log.read_text().splitlines()]
                    configs = [r for r in records if r.get('type') == 'configure_external_tools' and r.get('tools')]
                    self.assertEqual(len(configs), 3)
                    self.assertTrue(all(len(c['tools'][0]['name']) == 64 for c in configs))
                    transcripts = list(store.rglob('*.jsonl'))
                    self.assertTrue(transcripts)
                    for path in transcripts:
                        self.assertNotIn(SECRET, path.read_text(encoding='utf-8'))
                    self.assertNotIn(SECRET, json.dumps(frames))
                    self.assertNotIn(SECRET, native_log.read_text())
                    self.assertNotIn(SECRET, ''.join(bridge.errors))
                finally:
                    bridge.close()
                    for handle in handles:
                        Bridge.wait_handle_exit(handle)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--rpc', type=Path, required=True)
    args = parser.parse_args()
    McpProtocol.bridge = args.bridge.resolve()
    McpProtocol.rpc = args.rpc.resolve()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(McpProtocol)
    return not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful()


if __name__ == '__main__':
    if '--serve' in sys.argv:
        serve()
    else:
        raise SystemExit(main())
