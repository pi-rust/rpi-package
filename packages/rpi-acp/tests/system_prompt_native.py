#!/usr/bin/env python3
"""Native ACP systemPrompt acceptance; only the model endpoint is fake.
Run with --bridge PATH --native PATH --artifacts EMPTY_DIRECTORY.
Uses isolated homes, records binary hashes and model inputs. No installation.
"""
import argparse
import hashlib
import json
from pathlib import Path
import threading
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from protocol_regression import Bridge

REVISION = '1142c83bbfa9407f9f264d1e290ffafcd54eb79e'
OPT_IN = 'rpi/unstable-session-rfds'


def run(args, root, version):
    root.mkdir()
    workspace = root / 'workspace'
    workspace.mkdir()
    agent = root / 'agent'
    (agent / 'extensions').mkdir(parents=True)
    captures, frames = [], []

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            captures.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            events = [
                {'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': 'OK'}, 'finish_reason': None}]},
                {'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}]},
            ]
            data = (''.join('data: ' + json.dumps(e) + '\n\n' for e in events) + 'data: [DONE]\n\n').encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    (agent / 'models.json').write_text(json.dumps({'providers': {'system-local': {
        'api': 'openai-completions', 'baseUrl': f'http://127.0.0.1:{server.server_port}/v1',
        'apiKey': 'local-only', 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False,
        'input': ['text'], 'contextWindow': 1000000, 'maxTokens': 1024}]}}}), encoding='utf-8')
    (agent / 'settings.json').write_text(json.dumps({'defaultProvider': 'system-local',
        'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'}), encoding='utf-8')
    env = dict(RPI_CODING_AGENT_DIR=agent, RPI_EXTENSIONS_DIR=agent / 'extensions',
        RPI_OFFLINE='1', HOME=root, USERPROFILE=root)
    store = root / 'sessions'
    bridge = None

    def start(opt, revision=REVISION):
        nonlocal bridge
        bridge = Bridge(args.bridge, args.native, store, workspace, extra_env=env)
        caps = {'_meta': {OPT_IN: revision}} if opt else {}
        params = {'protocolVersion': 1, 'clientCapabilities': caps} if version == 1 else {
            'protocolVersion': 2, 'info': {'name': 'system-test', 'version': '1'}, 'capabilities': caps}
        result, _ = request('initialize', params)
        cap = result['agentCapabilities']['sessionCapabilities'] if version == 1 else result['capabilities']['session']
        enabled = opt and revision == REVISION
        assert ('systemPrompt' in cap) == enabled, cap
        if enabled:
            assert cap['systemPrompt'] == {'mode': {'override': {}}}, cap

    def request(method, params, error=None):
        reply, notes = bridge.request(method, params)
        frames.extend(notes + [reply])
        if error is not None:
            assert reply.get('error', {}).get('code') == error, reply
            return reply, notes
        assert 'result' in reply, reply
        return reply['result'], notes

    def setup(sid=None, mode=None, text=None):
        result = {'cwd': str(workspace), 'mcpServers': []}
        if sid:
            result['sessionId'] = sid
        if mode:
            result['systemPrompt'] = {'mode': mode, 'content': [{'type': 'text', 'text': text}]}
        return result

    def prompt(sid, marker, absent=None):
        _, notes = request('session/prompt', {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'Reply OK.'}]})
        if version == 2:
            while not any(n.get('params', {}).get('update', {}).get('state') == 'idle' for n in notes):
                frame = bridge.output.get(timeout=20)
                frames.append(frame)
                notes.append(frame)
        system = json.dumps([m for m in captures[-1]['messages'] if m['role'] in ('system', 'developer')])
        assert system.count(marker) == 1, system
        if absent:
            assert absent not in system, system

    try:
        start(False)
        for method in ['session/new', 'session/resume'] + (['session/load'] if version == 1 else []):
            request(method, setup('00000000-0000-4000-8000-000000000000', 'append', 'GATED_MARKER'), -32602)
        assert not list(store.glob('*/**/*.jsonl')), 'stable rejected request created a transcript'
        bridge.close()
        start(True, 'unsupported-revision')
        request('session/new', setup(mode='override', text='GATED_OVERRIDE_MARKER'), -32602)
        bridge.close()
        start(True)
        for value in [
            {'mode': 'replace', 'content': []},
            {'mode': 'append', 'content': [{'type': 'image', 'data': 'x', 'mimeType': 'image/png'}]},
            {'mode': 'append', 'content': [{'type': 'future'}]},
            {'mode': 'append', 'content': [{'type': 'text', 'text': 7}]},
            {'mode': 'append', 'content': [{'type': 'text', 'text': 'x' * (256 * 1024 + 1)}]},
            {'mode': 'append', 'content': [{'type': 'text', 'text': 'é' * (128 * 1024 + 1)}]},
            {'mode': 'append', 'content': [{'type': 'text', 'text': 'x' * (256 * 1024)},
                                           {'type': 'text', 'text': ''}]},
        ]:
            params = setup()
            params['systemPrompt'] = value
            request('session/new', params, -32602)
        assert not list(store.glob('*/**/*.jsonl')), 'invalid content created a transcript'
        result, _ = request('session/new', setup(mode='append', text='ACP_APPEND_MARKER'))
        sid = result['sessionId']
        prompt(sid, 'ACP_APPEND_MARKER')
        request('session/resume', setup(sid, 'append', 'ACP_APPEND_MARKER'))
        prompt(sid, 'ACP_APPEND_MARKER')
        if version == 1:
            request('session/load', setup(sid, 'append', 'ACP_APPEND_MARKER'))
            prompt(sid, 'ACP_APPEND_MARKER')
        request('session/resume', setup(sid, 'append', 'ACP_CHANGED_MARKER'))
        prompt(sid, 'ACP_CHANGED_MARKER', 'ACP_APPEND_MARKER')
        bridge.close()
        start(True)
        request('session/load' if version == 1 else 'session/resume', setup(sid))
        prompt(sid, 'ACP_CHANGED_MARKER', 'ACP_APPEND_MARKER')
        fork, _ = request('session/fork', setup(sid))
        prompt(fork['sessionId'], 'ACP_CHANGED_MARKER')
        fork_override, _ = request('session/fork', setup(sid, 'override', 'ACP_OVERRIDE_MARKER'))
        prompt(fork_override['sessionId'], 'ACP_OVERRIDE_MARKER', 'ACP_CHANGED_MARKER')
        request('session/load' if version == 1 else 'session/resume', setup(sid, 'override', 'ACP_LIVE_MARKER'))
        prompt(sid, 'ACP_LIVE_MARKER', 'ACP_CHANGED_MARKER')
        return {'version': version, 'result': 'passed', 'model_calls': len(captures)}
    finally:
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
    args.bridge, args.native, args.artifacts = args.bridge.resolve(), args.native.resolve(), args.artifacts.resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    report = {'revision': REVISION, 'binaries': {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in [args.bridge, args.native]}, 'runs': []}
    for version in [1, 2]:
        try:
            report['runs'].append(run(args, args.artifacts / f'v{version}', version))
        except Exception:
            report['runs'].append({'version': version, 'result': 'failed',
                                   'error': traceback.format_exc()})
    report['result'] = 'failed' if any(r['result'] == 'failed' for r in report['runs']) else 'passed'
    (args.artifacts / 'result.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps(report, indent=2))
    return int(report['result'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
