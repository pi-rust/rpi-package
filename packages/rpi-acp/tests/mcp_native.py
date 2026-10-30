#!/usr/bin/env python3
"""Real native ACP v1/v2 -> model-selected discovered stdio MCP acceptance.

python mcp_native.py --bridge PATH --native PATH --artifacts DIRECTORY
Only the loopback OpenAI endpoint is deterministic; native and MCP are genuine
subprocesses. Standard library only, no build/install or unrelated process kills.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import sys
import threading
import time
import traceback
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from client_tools_native import dump, require, text
from protocol_regression import Bridge

MODES = ['text', 'image', 'structured', 'tool-error']
SECRET = 'mcp-native-env-only-do-not-persist-75f98'


def png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    return base64.b64encode(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\xff')) + chunk(b'IEND', b'')).decode()


def serve():
    owner = os.environ['MCP_OWNER']
    log = Path(os.environ['MCP_LOG'])
    def record(value):
        with log.open('a', encoding='utf-8') as f:
            f.write(json.dumps(value) + '\n')
    record({'event': 'start', 'pid': os.getpid(), 'owner': owner})
    for line in sys.stdin:
        req = json.loads(line)
        if 'id' not in req:
            continue
        method = req['method']
        reply = {'jsonrpc': '2.0', 'id': req['id']}
        if method == 'initialize':
            result = {'protocolVersion': req['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'native-mcp-fixture', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'echo / punctuation', 'description': 'Discovered MCP acceptance tool. Select mode and owner.', 'inputSchema': {'type': 'object', 'properties': {'mode': {'type': 'string', 'enum': MODES + ['hold']}, 'owner': {'type': 'string'}}, 'required': ['mode', 'owner'], 'additionalProperties': False}}]}
        elif method == 'tools/call':
            args = req['params']['arguments']
            require(req['params']['name'] == 'echo / punctuation', req)
            require(args['owner'] == owner, 'cross-session MCP invocation')
            require(os.environ.get('MCP_SECRET') == SECRET, 'MCP environment not delivered')
            mode = args['mode']
            record({'event': 'call', 'owner': owner, 'mode': mode})
            if mode == 'hold':
                time.sleep(120)
            content = [{'type': 'text', 'text': 'MCP_RESULT_' + owner + '_' + mode}]
            if mode == 'image':
                content.append({'type': 'image', 'mimeType': 'image/png', 'data': png()})
            result = {'content': content, 'isError': mode == 'tool-error'}
            if mode == 'structured':
                result['structuredContent'] = {'owner': owner, 'value': 'STRUCTURED_' + owner, 'envReceived': True}
        else:
            reply['error'] = {'code': -32601, 'message': 'unsupported'}
            print(json.dumps(reply), flush=True)
            continue
        reply['result'] = result
        print(json.dumps(reply), flush=True)


def wait_for(predicate, detail, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.03)
    raise AssertionError(detail)


def records(path):
    return [json.loads(s) for s in path.read_text(encoding='utf-8').splitlines()] if path.exists() else []


def run(args, root, version):
    root.mkdir(parents=True)
    workspace = root / 'workspace'
    workspace.mkdir()
    agent = root / 'agent'
    (agent / 'extensions').mkdir(parents=True)
    captures, frames, errors, handles = [], [], [], {}
    fidelity_failures = set()
    lock = threading.Lock()
    provider_done = {owner: threading.Event() for owner in ['ALPHA', 'BETA']}

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            try:
                require(self.path == '/v1/chat/completions', self.path)
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                with lock:
                    captures.append(body)
                messages = body['messages']
                user = next(text(m) for m in reversed(messages) if m.get('role') == 'user' and 'OWNER_' in text(m))
                owner = 'ALPHA' if 'OWNER_ALPHA' in user else 'BETA'
                require('OWNER_' + owner in user, 'missing owner in provider input')
                tools = [t['function'] for t in body.get('tools', []) if 'Discovered MCP acceptance' in t['function'].get('description', '')]
                require(len(tools) == 1, 'actual discovered MCP tool schema missing')
                tool = tools[0]
                require(tool['parameters']['properties']['mode']['enum'] == MODES + ['hold'], tool)
                results = [m for m in messages if m.get('role') == 'tool' and m.get('tool_call_id', '').startswith(owner + '-')]
                for index, result in enumerate(results):
                    require('MCP_RESULT_' + owner + '_' + MODES[index] in text(result), result)
                    require('MCP_RESULT_' + ('BETA' if owner == 'ALPHA' else 'ALPHA') not in text(result), 'cross-session result')
                    if index == 2 and 'STRUCTURED_' + owner not in text(result):
                        fidelity_failures.add('structuredContent lost before provider: ' + owner)
                if 'HOLD' in user:
                    mode, callid = 'hold', owner + '-hold'
                elif len(results) == len(MODES):
                    require(png() in text(messages), 'PNG missing from subsequent provider input')
                    delta, finish = {'content': 'NATIVE_MCP_DONE_' + owner}, 'stop'
                    provider_done[owner].set()
                    mode = None
                else:
                    mode, callid = MODES[len(results)], owner + '-' + str(len(results))
                if mode:
                    delta = {'tool_calls': [{'index': 0, 'id': callid, 'type': 'function', 'function': {'name': tool['name'], 'arguments': json.dumps({'owner': owner, 'mode': mode})}}]}
                    finish = 'tool_calls'
                events = [{'choices': [{'index': 0, 'delta': {'role': 'assistant'}, 'finish_reason': None}]}, {'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]}, {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]}]
                data = (''.join('data: ' + json.dumps(e) + '\n\n' for e in events) + 'data: [DONE]\n\n').encode()
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception:
                errors.append(traceback.format_exc())
                self.send_error(500, 'fixture assertion failed')

    server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    dump(agent / 'models.json', {'providers': {'mcp-local': {'api': 'openai-completions', 'baseUrl': f'http://127.0.0.1:{server.server_port}/v1', 'apiKey': 'local-only', 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False, 'input': ['text', 'image'], 'contextWindow': 1000000, 'maxTokens': 1024}]}}})
    dump(agent / 'settings.json', {'defaultProvider': 'mcp-local', 'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'})
    env = {k: v for k, v in os.environ.items() if not any(s in k.upper() for s in ['TOKEN', 'SECRET', 'API_KEY']) and not k.startswith('RPI_')}
    env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / 'extensions'), RPI_OFFLINE='1', HOME=str(root), USERPROFILE=str(root))
    # Probe the supplied actual native executable, not a fixture or wrapper.
    probe = subprocess.Popen([str(args.native), '--mode', 'rpc'], cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8')
    try:
        out, err = probe.communicate('', timeout=20)
    except subprocess.TimeoutExpired:
        probe.kill()
        out, err = probe.communicate()
        raise AssertionError('native ready probe failed to exit on EOF')
    (root / 'native-probe.jsonl').write_text(out, encoding='utf-8')
    (root / 'native-probe-stderr.log').write_text(err, encoding='utf-8')
    ready = next(json.loads(line) for line in out.splitlines() if json.loads(line).get('type') == 'ready')
    require(ready.get('protocol') == 'rpi' and 'configure_external_tools' in text(ready), ready)
    bridge = None
    store = root / 'sessions'

    def request(method, params):
        reply, updates = bridge.request(method, params)
        frames.extend(updates + [reply])
        require('result' in reply, reply)
        return reply['result'], updates

    def idle(sid, since, stop='end_turn'):
        def found():
            return next((f for f in frames[since:] if f.get('method') == 'session/update' and f.get('params', {}).get('sessionId') == sid and f['params'].get('update', {}).get('state') == 'idle' and f['params']['update'].get('stopReason') == stop), None)
        deadline = time.monotonic() + 45
        while not found():
            require(time.monotonic() < deadline, 'no independent native settlement idle for ' + sid)
            frames.append(bridge.output.get(timeout=max(.01, deadline - time.monotonic())))
        return found()

    try:
        # Bridge helper merges the parent environment: temporarily replace it with
        # the sanitized environment only during spawn (before any provider calls).
        original = os.environ.copy()
        try:
            os.environ.clear()
            os.environ.update(env)
            bridge = Bridge(args.bridge, args.native, store, workspace, extra_env=env)
        finally:
            os.environ.clear()
            os.environ.update(original)
        init = {'protocolVersion': version, **({'clientCapabilities': {}} if version == 1 else {'info': {'name': 'mcp-native', 'version': '1'}, 'capabilities': {}})}
        initialized, _ = request('initialize', init)
        require(initialized['protocolVersion'] == version, initialized)
        sessions = {}
        for owner in ['ALPHA', 'BETA']:
            log = root / (owner + '-mcp.jsonl')
            config = {'name': 'Same human tools / punctuation', 'command': str(Path(sys.executable).resolve()), 'args': [str(Path(__file__).resolve()), '--serve'], 'env': [{'name': 'MCP_OWNER', 'value': owner}, {'name': 'MCP_LOG', 'value': str(log)}, {'name': 'MCP_SECRET', 'value': SECRET}]}
            if version == 2:
                config['type'] = 'stdio'
            created, _ = request('session/new', {'cwd': str(workspace), 'mcpServers': [config]})
            sessions[owner] = created['sessionId']
            pid = wait_for(lambda: next((r['pid'] for r in records(log) if r['event'] == 'start'), None), 'MCP did not start')
            handles[owner] = Bridge.open_pid_handle(pid)
        for owner, sid in sessions.items():
            since = len(frames)
            result, _ = request('session/prompt', {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'OWNER_' + owner + ' run all discovered MCP modes.'}]})
            if version == 1:
                require(result.get('stopReason') == 'end_turn', result)
            else:
                require('messageId' in result, result)
                idle(sid, since)
            require(provider_done[owner].is_set(), 'provider never consumed all tool results')
            require(not errors, errors)
        # A session close must reap only its own same-named MCP subprocess.
        request('session/close', {'sessionId': sessions['ALPHA']})
        Bridge.wait_handle_exit(handles.pop('ALPHA'))
        # Start a real blocking MCP call in BETA, then cancel independently of
        # v2's early prompt insertion response. EOF must also reap its MCP.
        since = len(frames)
        bridge.send({'jsonrpc': '2.0', 'id': 9001, 'method': 'session/prompt', 'params': {'sessionId': sessions['BETA'], 'prompt': [{'type': 'text', 'text': 'OWNER_BETA HOLD'}]}})
        wait_for(lambda: any(r.get('mode') == 'hold' for r in records(root / 'BETA-mcp.jsonl')), 'remaining session process not alive/hold not reached')
        bridge.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': sessions['BETA']}})
        if version == 2:
            idle(sessions['BETA'], since, 'cancelled')
        else:
            reply, updates = bridge.wait_response(9001)
            frames.extend(updates + [reply])
            require(reply.get('result', {}).get('stopReason') == 'cancelled', reply)
        bridge.close()
        Bridge.wait_handle_exit(handles.pop('BETA'))
        transcripts = list(store.rglob('*.jsonl'))
        require(len(transcripts) == 2, transcripts)
        for owner, sid in sessions.items():
            paths = list((store / sid).rglob('*.jsonl'))
            require(len(paths) == 1, paths)
            content = paths[0].read_text(encoding='utf-8')
            require(SECRET not in content, 'MCP secret persisted in native transcript')
            messages = [e['message'] for e in records(paths[0]) if e.get('type') == 'message']
            for index, mode in enumerate(MODES):
                result = next(m for m in messages if m.get('role') == 'toolResult' and m.get('toolCallId') == owner + '-' + str(index))
                require('MCP_RESULT_' + owner + '_' + mode in text(result), result)
                require(bool(result.get('isError')) == (mode == 'tool-error'), result)
                if mode == 'image':
                    require(png() in text(result), 'native transcript lost PNG')
                if mode == 'structured' and 'STRUCTURED_' + owner not in text(result):
                    fidelity_failures.add('structuredContent lost in native transcript: ' + owner)
            require(any(m.get('role') == 'assistant' and 'NATIVE_MCP_DONE_' + owner in text(m) for m in messages), 'native assistant did not finish')
            require('MCP_RESULT_' + ('BETA' if owner == 'ALPHA' else 'ALPHA') not in content, 'cross-session native transcript')
        require(SECRET not in json.dumps(frames) and SECRET not in ''.join(bridge.errors), 'secret leaked to ACP')
        return {'version': version, 'result': 'failed' if fidelity_failures else 'passed', 'failures': sorted(fidelity_failures), 'model_calls': len(captures), 'sessions': sessions, 'native_configure_external_tools_advertised': True, 'native_results_verified': 8, 'close_abort_eof_cleanup': True, 'independent_v2_idle': version == 2}
    finally:
        if bridge:
            try:
                bridge.close()
            finally:
                (root / 'acp-stderr.log').write_text(''.join(bridge.errors), encoding='utf-8')
        for handle in handles.values():
            Bridge.wait_handle_exit(handle, timeout=10)
        server.shutdown()
        server.server_close()
        dump(root / 'model-inputs.json', captures)
        dump(root / 'acp-frames.json', frames)
        dump(root / 'model-errors.json', errors)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    args.bridge, args.native = args.bridge.resolve(), args.native.resolve()
    root = args.artifacts.resolve()
    root.mkdir(parents=True, exist_ok=True)
    report = {'result': 'failed', 'command': subprocess.list2cmdline([sys.executable] + sys.argv), 'binaries': {}, 'tests': []}
    try:
        for name in ['bridge', 'native']:
            path = getattr(args, name)
            report['binaries'][name] = {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'size': path.stat().st_size, 'mtime_ns': path.stat().st_mtime_ns}
        for version in [1, 2]:
            try:
                report['tests'].append(run(args, root / ('v' + str(version)), version))
            except Exception:
                report['tests'].append({'version': version, 'result': 'failed', 'error': traceback.format_exc()})
        if all(t['result'] == 'passed' for t in report['tests']):
            report['result'] = 'passed'
    except Exception:
        report['error'] = traceback.format_exc()
    dump(root / 'result.json', report)
    print(json.dumps(report, indent=2))
    return 0 if report['result'] == 'passed' else 1


if __name__ == '__main__':
    if '--serve' in sys.argv:
        serve()
    else:
        raise SystemExit(main())
