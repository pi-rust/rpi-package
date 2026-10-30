#!/usr/bin/env python3
"""Real isolated native ACP session-RFD acceptance (v1 and v2).
No fake native RPC: only the model endpoint is deterministic loopback HTTP.
Usage: session_rfd_protocol.py --bridge PATH --native PATH --artifacts PATH
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import threading
import time
import traceback
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from protocol_regression import Bridge

REVISION = '1142c83bbfa9407f9f264d1e290ffafcd54eb79e'
OPT_IN = 'rpi/unstable-session-rfds'


def check(condition, detail):
    if not condition:
        raise AssertionError(detail)


def run(args, root, version):
    root.mkdir()
    workspace = root / 'workspace'
    workspace.mkdir()
    extra = root / 'extra-root'
    extra.mkdir()
    agent = root / 'agent'
    (agent / 'extensions').mkdir(parents=True)
    captures = []
    frames = []
    holding = threading.Event()
    release = threading.Event()

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            captures.append(body)
            if 'HOLD_ARCHIVE' in json.dumps(body['messages'][-1]):
                holding.set()
                release.wait(20)
            events = [
                {'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': 'Native fork response'}, 'finish_reason': None}]},
                {'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}]},
            ]
            data = (''.join('data: ' + json.dumps(e) + '\n\n' for e in events) + 'data: [DONE]\n\n').encode()
            try:
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    (agent / 'models.json').write_text(json.dumps({'providers': {'rfd-local': {
        'api': 'openai-completions', 'baseUrl': f'http://127.0.0.1:{server.server_port}/v1',
        'apiKey': 'local-only', 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False,
        'input': ['text'], 'contextWindow': 1000000, 'maxTokens': 1024}]}}}), encoding='utf-8')
    (agent / 'settings.json').write_text(json.dumps({'defaultProvider': 'rfd-local',
        'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'}), encoding='utf-8')
    env = dict(RPI_CODING_AGENT_DIR=agent, RPI_EXTENSIONS_DIR=agent / 'extensions',
        RPI_OFFLINE='1', HOME=root, USERPROFILE=root)
    store = root / 'sessions'
    bridge = None

    def start(opt=True):
        b = Bridge(args.bridge, args.native, store, workspace, extra_env=env)
        caps = {'_meta': {OPT_IN: REVISION}} if opt else {}
        params = {'protocolVersion': version, 'clientCapabilities': caps} if version == 1 else {
            'protocolVersion': 2, 'info': {'name': 'rfd-native-test', 'version': '1'}, 'capabilities': caps}
        reply, notifications = b.request('initialize', params)
        frames.extend(notifications + [reply])
        check('result' in reply, reply)
        cap = reply['result']['agentCapabilities']['sessionCapabilities'] if version == 1 else reply['result']['capabilities']['session']
        for key in ['fork', 'setTitle', 'archive']:
            check((key in cap) == opt, cap)
        return b

    def request(method, params=None, error=None):
        reply, notifications = bridge.request(method, params)
        frames.extend(notifications + [reply])
        if error is not None:
            check(reply.get('error', {}).get('code') == error, reply)
            return reply, notifications
        check('result' in reply, reply)
        if version == 2 and method == 'session/prompt':
            # v2 acknowledges the durable user insertion, not completion.
            check(bool(reply['result'].get('messageId')), reply)
            while not any(m.get('method') == 'session/update' and
                          m.get('params', {}).get('update', {}).get('state') == 'idle'
                          for m in notifications):
                frame = bridge.output.get(timeout=15)
                frames.append(frame)
                notifications.append(frame)
        return reply['result'], notifications

    def listing(archived=None, cwd=None):
        params = {}
        if archived is not None:
            params['archived'] = archived
        if cwd:
            params['cwd'] = str(cwd)
        seen = []
        pages = 0
        while True:
            result, _ = request('session/list', params)
            seen.extend(result['sessions'])
            pages += 1
            if not result.get('nextCursor'):
                break
            params['cursor'] = result['nextCursor']
        check(len({s['sessionId'] for s in seen}) == len(seen), 'duplicate paging IDs')
        return seen, pages

    try:
        bridge = start(False)
        for method in ['session/set_title', 'session/archive', 'session/unarchive', 'session/fork']:
            request(method, {'sessionId': str(uuid.uuid4()), 'title': 'x', 'cwd': str(workspace)}, -32601)
        request('session/list', {'archived': 'only'}, -32602)
        bridge.close()
        bridge = start()
        created, _ = request('session/new', {'cwd': str(workspace), 'mcpServers': [], 'additionalDirectories': [str(extra)]})
        sid = created['sessionId']
        title, _ = request('session/set_title', {'sessionId': sid, 'title': '  Chosen\n title  '})
        check(title == {'title': 'Chosen title'}, title)
        for invalid in [None, '', '  ', True, 'x' * 257]:
            request('session/set_title', {'sessionId': sid, 'title': invalid}, -32602)
        for method in ['session/set_title', 'session/archive', 'session/unarchive', 'session/fork']:
            request(method, {'sessionId': str(uuid.uuid4()), 'title': 'x', 'cwd': str(workspace)}, -32002)
        request('session/prompt', {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'SOURCE_MARKER'}]})
        source = next((store / sid).rglob('*.jsonl'))
        original = source.read_bytes()
        fork, _ = request('session/fork', {'sessionId': sid, 'cwd': str(workspace), 'mcpServers': [], 'additionalDirectories': [str(extra)]})
        fid = fork['sessionId']
        check(fid != sid, fork)
        response, _ = request('session/prompt', {'sessionId': fid, 'prompt': [{'type': 'text', 'text': 'FORK_MARKER'}]})
        check(response.get('stopReason') == 'end_turn' if version == 1 else bool(response.get('messageId')), response)
        check('SOURCE_MARKER' in json.dumps(captures[-1]) and 'FORK_MARKER' in json.dumps(captures[-1]), captures[-1])
        check(source.read_bytes() == original, 'fork changed source transcript')
        check(captures[-1]['model'] == 'fixture', captures[-1])
        listed, _ = listing('include')
        check(next(s for s in listed if s['sessionId'] == sid)['title'] == 'Chosen title', listed)
        check(any(extra.samefile(p) for p in next(s for s in listed if s['sessionId'] == fid)['additionalDirectories']), listed)
        # Archive must cancel the real in-flight native model request and unload.
        bridge.send({'jsonrpc': '2.0', 'id': 9001, 'method': 'session/prompt', 'params': {
            'sessionId': fid, 'prompt': [{'type': 'text', 'text': 'HOLD_ARCHIVE'}]}})
        check(holding.wait(10), 'native prompt did not reach provider')
        _, notifications = request('session/archive', {'sessionId': fid})
        check(any(m.get('method') == 'session/update' and m['params']['update'].get('archived') is True for m in notifications), notifications)
        check(any(m.get('id') == 9001 for m in notifications) if version == 1 else any(
            m.get('method') == 'session/update' and m.get('params', {}).get('update', {}).get('state') == 'idle'
            for m in notifications), 'archive returned before active prompt settled')
        release.set()
        request('session/prompt', {'sessionId': fid, 'prompt': [{'type': 'text', 'text': 'must be closed'}]}, -32602)
        request('session/archive', {'sessionId': fid})
        check(fid not in {s['sessionId'] for s in listing()[0]}, 'default includes archive')
        check({s['sessionId'] for s in listing('only')[0]} == {fid}, 'only filter')
        request('session/list', {'archived': True}, -32602)
        # Race mutations: both fields must survive read-modify-write.
        for n in range(5):
            bridge.send({'jsonrpc': '2.0', 'id': 9100, 'method': 'session/set_title', 'params': {'sessionId': sid, 'title': f'Concurrent {n}'}})
            bridge.send({'jsonrpc': '2.0', 'id': 9101, 'method': 'session/archive', 'params': {'sessionId': sid}})
            replies = {}
            while len(replies) < 2:
                frame = bridge.output.get(timeout=15)
                frames.append(frame)
                if frame.get('id') in [9100, 9101]:
                    replies[frame['id']] = frame
            check(all('result' in r for r in replies.values()), replies)
            row = next(s for s in listing('only')[0] if s['sessionId'] == sid)
            check(row['title'] == f'Concurrent {n}' and row['archived'], row)
            request('session/unarchive', {'sessionId': sid})
        bridge.close()
        bridge = start()
        check(fid in {s['sessionId'] for s in listing('only')[0]}, 'archive lost on restart')
        check(next(s for s in listing()[0] if s['sessionId'] == sid)['title'] == 'Concurrent 4', 'title lost on restart')
        request('session/unarchive', {'sessionId': fid})
        request('session/unarchive', {'sessionId': fid})
        request('session/prompt', {'sessionId': fid, 'prompt': [{'type': 'text', 'text': 'unarchive must not load'}]}, -32602)
        request('session/resume', {'sessionId': fid, 'cwd': str(workspace), **({'replayFrom': None} if version == 2 else {})})
        request('session/prompt', {'sessionId': fid, 'prompt': [{'type': 'text', 'text': 'AFTER_RESTART'}]})
        check(source.read_bytes() == original, 'source changed after restart/fork prompts')
        # Native-format saved histories exercise >50 wire page size, mixed archive
        # state and cwd. They are real native files, not fake-RPC responses.
        for n in range(105):
            fake_id = str(uuid.uuid4())
            directory = store / fake_id / 'cwd'
            directory.mkdir(parents=True)
            header = {'kind': 'header', 'version': 4, 'id': fake_id, 'createdAt': n,
                      'cwd': str(extra if n % 5 == 0 else workspace)}
            (directory / f'{n}_{fake_id}.jsonl').write_text(json.dumps(header) + '\n', encoding='utf-8')
            if n % 2 == 0:
                request('session/archive', {'sessionId': fake_id})
        all_rows, pages = listing('include')
        check(len(all_rows) == 107 and pages == 3, (len(all_rows), pages))
        only, _ = listing('only', workspace)
        check(only and all(s['archived'] and Path(s['cwd']).resolve() == workspace.resolve() for s in only), only)
        filtered, _ = listing('exclude', workspace)
        check(all(not s['archived'] and Path(s['cwd']).resolve() == workspace.resolve() for s in filtered), filtered)
        first, _ = request('session/list', {'archived': 'include'})
        request('session/list', {'archived': 'only', 'cursor': first['nextCursor']}, -32602)
        request('session/list', {'archived': 'include', 'cwd': str(extra), 'cursor': first['nextCursor']}, -32602)
        request('session/archive', {'sessionId': fid})
        request('session/delete', {'sessionId': fid})
        request('session/delete', {'sessionId': fid})
        request('session/unarchive', {'sessionId': fid}, -32002)
        for mode in ['include', 'only', 'exclude']:
            check(fid not in {s['sessionId'] for s in listing(mode)[0]}, 'delete is archive')
        check(not list(store.glob('.fork-*')), 'staging leak')
        return {'version': version, 'result': 'passed', 'model_calls': len(captures), 'paging_rows': len(all_rows), 'paging_pages': pages}
    finally:
        release.set()
        if bridge:
            bridge.close()
        server.shutdown()
        server.server_close()
        (root / 'frames.json').write_text(json.dumps(frames, indent=2), encoding='utf-8')
        (root / 'model-inputs.json').write_text(json.dumps(captures, indent=2), encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    args.bridge = args.bridge.resolve()
    args.native = args.native.resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    report = {'revision': REVISION, 'binaries': {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in [args.bridge, args.native]}, 'runs': []}
    try:
        for version in [1, 2]:
            report['runs'].append(run(args, args.artifacts / f'v{version}', version))
    except Exception:
        report['error'] = traceback.format_exc()
    (args.artifacts / 'result.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps(report, indent=2))
    return int('error' in report)


if __name__ == '__main__':
    raise SystemExit(main())
