#!/usr/bin/env python3
"""Actual native ACP incoming modalities and secondary workspace acceptance.

Stdlib only. No build/install, live AI, permission bypass or runtime edits.
Required: --native EXE --bridge EXE --artifacts NEW_ABSOLUTE_C_DIRECTORY.
Optional expected hashes are explicit pins, never inferred/updated automatically.
All frames, controlled provider inputs, case failures and isolated homes survive.
An advertised modality silently dropped/rejected is a failure, not a skip.
"""
import argparse
import base64
import hashlib
import io
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
import wave
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REVISION = '1142c83bbfa9407f9f264d1e290ffafcd54eb79e'
OPT_IN = 'rpi/unstable-session-rfds'


def check(value, detail):
    if not value:
        raise AssertionError(detail)


def dump(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False), encoding='utf-8')


def png():
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    # Known opaque red pixel; exact compressed bytes must survive the provider.
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\xff')) + chunk(b'IEND', b'')


def wav():
    out = io.BytesIO()
    with wave.open(out, 'wb') as f:
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(8000)
        f.writeframes(b'\x00\x00' * 80)
    return out.getvalue()


def text(value):
    return json.dumps(value, ensure_ascii=False)


class Peer:
    """No protocol_regression.Bridge: that helper disables permission gating."""
    def __init__(self, args, root, env):
        self.frames, self.errors, self.permissions = [], [], []
        self.q, self.seq = queue.Queue(), 0
        self.p = subprocess.Popen([str(args.bridge)], cwd=root / 'launch', env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding='utf-8')
        def reader():
            for line in self.p.stdout:
                try:
                    self.q.put(json.loads(line))
                except Exception:
                    self.q.put({'invalid_stdout': line})
            self.q.put({'eof': True})
        threading.Thread(target=reader, daemon=True).start()
        threading.Thread(target=lambda: self.errors.extend(self.p.stderr), daemon=True).start()

    def send(self, method, params):
        self.seq += 1
        self.write({'jsonrpc': '2.0', 'id': self.seq, 'method': method, 'params': params})
        return self.seq

    def write(self, value):
        self.p.stdin.write(json.dumps(value) + '\n')
        self.p.stdin.flush()

    def receive(self, deadline):
        f = self.q.get(timeout=max(.01, deadline - time.monotonic()))
        self.frames.append(f)
        check(not f.get('eof') and 'invalid_stdout' not in f, f)
        if 'id' in f and 'method' in f:
            check(f['method'] == 'session/request_permission', ('unexpected client tool/interaction; must use native FS', f))
            options = [o for o in f['params']['options'] if o['kind'] == 'allow_once']
            check(len(options) == 1, f)
            self.permissions.append(f)
            self.write({'jsonrpc': '2.0', 'id': f['id'], 'result': {
                'outcome': {'outcome': 'selected', 'optionId': options[0]['optionId']}}})
        return f

    def wait(self, rid, timeout=25):
        deadline = time.monotonic() + timeout
        while True:
            f = self.receive(deadline)
            if f.get('id') == rid and 'method' not in f:
                return f

    def request(self, method, params, timeout=25):
        return self.wait(self.send(method, params), timeout)

    def result(self, method, params):
        f = self.request(method, params)
        check('result' in f, f)
        return f['result']

    def settled(self, sid, since, timeout=25):
        deadline = time.monotonic() + timeout
        while True:
            found = [f['params']['update'] for f in self.frames[since:]
                if f.get('method') == 'session/update' and f['params']['sessionId'] == sid
                and f['params']['update'].get('state') == 'idle']
            if found:
                return found[-1]
            self.receive(deadline)

    def close(self):
        if self.p.poll() is None:
            self.p.stdin.close()
            try:
                self.p.wait(timeout=20)
            except subprocess.TimeoutExpired:
                # Only this fixture's owned bridge; no unrelated process kill.
                self.p.kill()
                self.p.wait(timeout=5)
                raise AssertionError('owned bridge failed bounded EOF shutdown')
        check(self.p.returncode == 0, ('bridge exit', self.p.returncode, self.errors))


def run(args, root, version):
    root.mkdir()
    for name in ('launch', 'agent/extensions', 'temp', 'home', 'appdata'):
        (root / name).mkdir(parents=True)
    captures, http_requests, model_errors, cases, roots = [], [], [], [], {}
    lock = threading.Lock()
    peer = None

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *unused):
            pass

        def do_GET(self):
            http_requests.append({'method': 'GET', 'path': self.path})
            self.send_error(404, 'must not fetch incoming resource URI')

        def do_POST(self):
            try:
                check(self.path == '/v1/chat/completions', self.path)
                size = int(self.headers['Content-Length'])
                check(size < 8 * 1024 * 1024, 'provider input exceeds fixture bound')
                body = json.loads(self.rfile.read(size))
                with lock:
                    captures.append(body)
                users = [m for m in body['messages'] if m['role'] == 'user']
                latest = text(users[-1])
                matches = [d for marker, d in roots.items() if marker in latest]
                delta, finish = {'content': 'LOCAL_NATIVE_OK'}, 'stop'
                if matches:
                    check(len(matches) == 1, 'cross workspace marker')
                    d = matches[0]
                    results = {m.get('tool_call_id'): m for m in body['messages'] if m['role'] == 'tool'}
                    steps = [('read', {'path': str(d['b'] / 'sentinel.txt')}, d['sentinel']),
                             ('write', {'path': str(d['b'] / 'written.txt'), 'content': d['written']}, 'Successfully wrote'),
                             ('read', {'path': str(d['b'] / 'written.txt')}, d['written']),
                             # A forged, valid absolute cwd in model arguments must not
                             # override the caller's real native root cwd.
                             ('read', {'path': 'cwd-sentinel.txt', 'cwd': str(d['c'])}, d['cwd_marker'])]
                    stage = 0
                    while d['marker'] + '-' + str(stage) in results:
                        check(stage < len(steps), 'unexpected tool result')
                        actual = text(results[d['marker'] + '-' + str(stage)])
                        check(steps[stage][2].lower() in actual.lower(), ('native tool result', stage, actual))
                        check(d['outside_marker'] not in actual, 'model cwd overrode caller cwd')
                        for other in roots.values():
                            if other is not d:
                                check(other['sentinel'] not in actual and other['written'] not in actual, 'cross-root filesystem result')
                        d['stages'].add(stage)
                        stage += 1
                    if stage < len(steps):
                        name, arguments, _ = steps[stage]
                        check(name in [t['function']['name'] for t in body.get('tools', [])], ('native tool absent', name))
                        delta = {'tool_calls': [{'index': 0, 'id': d['marker'] + '-' + str(stage), 'type': 'function',
                            'function': {'name': name, 'arguments': json.dumps(arguments)}}]}
                        finish = 'tool_calls'
                events = [{'choices': [{'index': 0, 'delta': {'role': 'assistant', **delta}, 'finish_reason': None}]},
                          {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]}]
                data = (''.join('data: ' + json.dumps(e) + '\n\n' for e in events) + 'data: [DONE]\n\n').encode()
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            except Exception:
                model_errors.append(traceback.format_exc())
                self.send_error(500, 'local fixture assertion')

    server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    agent = root / 'agent'
    dump(agent / 'models.json', {'providers': {'modalities-local': {
        'api': 'openai-completions', 'baseUrl': f'http://127.0.0.1:{server.server_port}/v1',
        'apiKey': 'local-only', 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False,
        'input': ['text', 'image'], 'contextWindow': 1000000, 'maxTokens': 1024}]}}})
    dump(agent / 'settings.json', {'defaultProvider': 'modalities-local', 'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'})
    env = {k: v for k, v in os.environ.items() if not k.startswith(('RPI_', 'PI_'))
        and not any(s in k.upper() for s in ('TOKEN', 'SECRET', 'API_KEY'))}
    env.update(RPI_BIN=str(args.native), RPI_ACP_SESSION_DIR=str(root / 'sessions'),
        RPI_ACP_NO_AUTO_REGISTER_ZED='1', RPI_CODING_AGENT_DIR=str(agent),
        RPI_EXTENSIONS_DIR=str(agent / 'extensions'), RPI_OFFLINE='1',
        HOME=str(root / 'home'), USERPROFILE=str(root / 'home'),
        APPDATA=str(root / 'appdata'), LOCALAPPDATA=str(root / 'appdata'),
        TEMP=str(root / 'temp'), TMP=str(root / 'temp'),
        XDG_CONFIG_HOME=str(root / 'home'), XDG_CACHE_HOME=str(root / 'home'),
        XDG_DATA_HOME=str(root / 'home'))
    dump(root / 'environment.json', {k: v for k, v in env.items() if k.startswith('RPI_') or k in ('HOME', 'TEMP', 'TMP', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA')})

    def start(opt=True):
        nonlocal peer
        peer = Peer(args, root, env)
        caps = {'_meta': {OPT_IN: REVISION}} if opt else {}
        # No native/client fs shortcut, bypass flag, or claimed permission capability.
        params = {'protocolVersion': version, **({'clientCapabilities': caps} if version == 1 else {
            'info': {'name': 'modalities-native', 'version': '1'}, 'capabilities': caps})}
        value = peer.result('initialize', params)
        check(value['protocolVersion'] == version, value)
        return value

    all_frames, all_errors = [], []

    def case(name, kind, fn):
        since, before = len(all_frames) + len(peer.frames), len(captures)
        try:
            detail = fn()
            cases.append({'name': name, 'kind': kind, 'result': 'passed', 'detail': detail})
        except Exception:
            cases.append({'name': name, 'kind': kind, 'result': 'failed', 'error': traceback.format_exc()})
        cases[-1].update(frame_start=since, frame_end=len(all_frames) + len(peer.frames), model_start=before, model_end=len(captures))
        dump(root / 'cases.json', cases)

    def new(cwd=None, **extra):
        return peer.result('session/new', {'cwd': str(cwd or root / 'launch'), 'mcpServers': [], **extra})['sessionId']

    def prompt(sid, blocks):
        since = len(peer.frames)
        response = peer.request('session/prompt', {'sessionId': sid, 'prompt': blocks})
        if version == 2 and 'result' in response:
            terminal = peer.settled(sid, since)
            check(terminal.get('stopReason') == 'end_turn', terminal)
        elif 'result' in response:
            check(response['result'].get('stopReason') == 'end_turn', response)
        return response

    def positive(blocks, verify):
        sid = new()
        before = len(captures)
        response = prompt(sid, blocks)
        check('result' in response, response)
        check(len(captures) == before + 1, ('expected exactly one controlled model call', before, len(captures)))
        user = [m for m in captures[-1]['messages'] if m['role'] == 'user'][-1]
        verify(user)
        return {'sessionId': sid, 'provider_user': user}

    def negative(blocks):
        sid = new()
        before = len(captures)
        response = prompt(sid, blocks)
        rejected = 'error' in response and len(captures) == before
        # Always attempt same-session recovery, even when the assertion fails.
        after_input = len(captures)
        recovery = prompt(sid, [{'type': 'text', 'text': 'RECOVERY_VALID_TEXT'}])
        recovered = ('result' in recovery and len(captures) == after_input + 1
            and 'RECOVERY_VALID_TEXT' in text([m for m in captures[-1]['messages'] if m['role'] == 'user'][-1]))
        dump(root / ('recovery-' + sid + '.json'), {'input_reply': response,
            'input_model_calls': after_input - before, 'recovery_reply': recovery,
            'same_session_followup': recovered})
        check(recovered, ('same-session recovery failed', recovery))
        check(rejected, ('invalid/unsupported input was not rejected before model call', response, len(captures) - before - 1))
        return {'rejection': response, 'same_session_followup': True}

    try:
        start(False)
        def setup_gates():
            before = len(captures)
            response = peer.request('session/new', {'cwd': str(root / 'launch'), 'mcpServers': [],
                'systemPrompt': {'mode': 'override', 'content': [{'type': 'text', 'text': 'UNNEGOTIATED'}]}})
            check(response.get('error', {}).get('code') == -32602 and len(captures) == before, response)
            return {'unstable_system_prompt_rejected_without_opt_in': True}
        case('stable-session-rfd-opt-in-gate', 'negative', setup_gates)
        peer.close()
        all_frames.extend(peer.frames)
        all_errors.extend(peer.errors)
        initialized = start()
        dump(root / 'initialize.json', initialized)
        cap = initialized['agentCapabilities']['promptCapabilities'] if version == 1 else initialized['capabilities']['session']['prompt']
        image = {'type': 'image', 'data': base64.b64encode(png()).decode(), 'mimeType': 'image/png'}
        audio = {'type': 'audio', 'data': base64.b64encode(wav()).decode(), 'mimeType': 'audio/wav'}

        def image_verify(user):
            parts = user['content']
            check(isinstance(parts, list), user)
            media = [p for p in parts if p['type'] == 'image_url']
            check(len(media) == 1, user)
            url = media[0]['image_url']['url']
            check(url.startswith('data:image/png;base64,'), ('MIME lost', url))
            check(base64.b64decode(url.split(',', 1)[1], validate=True) == png(), 'PNG bytes/pixel lost')

        case('incoming-image-bytes-mime', 'positive', lambda: positive([image], image_verify))

        def mixed_verify(user):
            image_verify(user)
            parts = user['content']
            kinds = [p['type'] for p in parts]
            check(kinds == ['text', 'image_url', 'text'], ('mixed ACP order lost', kinds, user))
            check(parts[0]['text'] == 'ORDER_BEFORE' and parts[2]['text'] == 'ORDER_AFTER', user)
        case('mixed-text-image-order', 'positive', lambda: positive([
            {'type': 'text', 'text': 'ORDER_BEFORE'}, image, {'type': 'text', 'text': 'ORDER_AFTER'}], mixed_verify))

        def audio_verify(user):
            parts = user['content']
            check(isinstance(parts, list), user)
            media = [p for p in parts if p['type'] == 'input_audio']
            check(len(media) == 1, ('audio not forwarded to controlled provider', user))
            payload = media[0]['input_audio']
            check(payload['format'] == 'wav', ('audio/wav MIME mapping lost', payload))
            check(base64.b64decode(payload['data'], validate=True) == wav(), 'WAV bytes lost')
            check('audio' in cap and cap['audio'] is not False, ('working audio not advertised', cap))
        case('incoming-audio-wav-bytes-format', 'positive', lambda: positive([audio], audio_verify))
        uri = f'http://127.0.0.1:{server.server_port}/never-fetch'
        def resource_verify(user):
            value = text(user)
            check('EMBEDDED_USER_RESOURCE' in value and uri in value, user)
            check(not http_requests, ('server fetched caller resource URI', http_requests))
        case('embedded-text-resource-no-fetch', 'positive', lambda: positive([{
            'type': 'resource', 'resource': {'uri': uri, 'mimeType': 'text/plain', 'text': 'EMBEDDED_USER_RESOURCE'}}], resource_verify))
        for alias in ('resource_link', 'resourceLink'):
            def link_test(alias=alias):
                def verify(user):
                    check('@CALLER_ALIAS' in text(user), user)
                    check(not http_requests, http_requests)
                if alias == 'resource_link':
                    return positive([{'type': alias, 'uri': uri, 'name': 'CALLER_ALIAS'}], verify)
                return negative([{'type': alias, 'uri': uri, 'name': 'CALLER_ALIAS'}])
            case(alias + '-alias-no-fetch', 'positive' if alias == 'resource_link' else 'negative', link_test)
        invalids = {
            'unknown-content': [{'type': 'future'}],
            'empty-image': [{**image, 'data': ''}],
            'empty-audio-mime': [{**audio, 'mimeType': ''}],
            'wrong-text-type': [{'type': 'text', 'text': 7}],
            'invalid-image-base64': [{**image, 'data': '%%%INVALID_BASE64%%%'}],
            'invalid-audio-base64': [{**audio, 'data': '%%%INVALID_BASE64%%%'}],
            'embedded-binary-resource': [{'type': 'resource', 'resource': {'uri': uri, 'mimeType': 'application/octet-stream', 'blob': 'AAEC'}}],
        }
        for name, blocks in invalids.items():
            case(name, 'negative', lambda blocks=blocks: negative(blocks))

        # Two independent live native roots, each with A (cwd), B (extra), C
        # (outside/deception). No outside-denial claim: roots are not a sandbox.
        for owner in ('ALPHA', 'BETA'):
            d = {k: root / (owner + '-' + k.upper()) for k in ('a', 'b', 'c')}
            for path in d.values():
                path.mkdir()
            d.update(marker='WORKSPACE_' + owner, sentinel='SECOND_ROOT_' + owner,
                written='REAL_NATIVE_WRITE_' + owner, cwd_marker='CALLER_CWD_' + owner,
                outside_marker='FORGED_CWD_' + owner, stages=set())
            (d['b'] / 'sentinel.txt').write_text(d['sentinel'], encoding='utf-8')
            (d['a'] / 'cwd-sentinel.txt').write_text(d['cwd_marker'], encoding='utf-8')
            (d['c'] / 'cwd-sentinel.txt').write_text(d['outside_marker'], encoding='utf-8')
            roots[d['marker']] = d

        def directories():
            for d in roots.values():
                d['sid'] = new(d['a'], additionalDirectories=[str(d['b'])])
            pending = {}
            since = len(peer.frames)
            for d in roots.values():
                rid = peer.send('session/prompt', {'sessionId': d['sid'], 'prompt': [{'type': 'text', 'text': d['marker']}]})
                pending[rid] = d
            deadline = time.monotonic() + 60
            while pending:
                f = peer.receive(deadline)
                if f.get('id') in pending and 'method' not in f:
                    check('result' in f, f)
                    d = pending.pop(f['id'])
                    if version == 1:
                        check(f['result'].get('stopReason') == 'end_turn', f)
            if version == 2:
                for d in roots.values():
                    check(peer.settled(d['sid'], since).get('stopReason') == 'end_turn', 'v2 failed')
            check(not model_errors, model_errors)
            for d in roots.values():
                check(d['stages'] == {0, 1, 2, 3}, ('native FS stages missing', d['stages']))
                check((d['b'] / 'written.txt').read_text(encoding='utf-8') == d['written'], 'real B write missing')
                check(not (d['a'] / 'written.txt').exists() and not (d['c'] / 'written.txt').exists(), 'wrong write cwd')
                matching = [b for b in captures if d['marker'] in text([m for m in b['messages'] if m['role'] == 'user'])]
                systems = text([m for m in matching[0]['messages'] if m['role'] in ('system', 'developer')])
                # JSON escapes Windows separators; normalize for path evidence.
                normalized = systems.replace('\\\\', '/').replace('\\', '/').lower()
                check(d['a'].as_posix().lower() in normalized, ('caller cwd missing from provider system', systems))
                check(d['b'].as_posix().lower() in normalized, ('extra root missing from provider system', systems))
            return {'parallel_distinct_native_roots': True, 'real_file_operations': 8, 'permission_requests': len(peer.permissions)}
        case('additional-directories-parallel-native-read-write-cwd', 'positive', directories)

        def invalid_roots():
            before = len(captures)
            for directories in (['relative-root'], [str(root / 'missing')], [str(root / 'agent' / 'models.json')], [7]):
                response = peer.request('session/new', {'cwd': str(root / 'launch'), 'mcpServers': [],
                    'additionalDirectories': directories})
                check(response.get('error', {}).get('code') == -32602, response)
            check(len(captures) == before, 'invalid roots called provider')
            return {'invalid_root_setups_rejected': 4}
        case('additional-directories-absolute-existing-directory-gate', 'negative', invalid_roots)

        # Save metadata evidence, restart the real bridge, omit extra roots on
        # resume: stored metadata must restore roots without changing root cwd.
        def reload_roots():
            metadata = list((root / 'sessions').rglob('*.json'))
            dump(root / 'metadata-before-restart.json', {str(p): json.loads(p.read_text(encoding='utf-8')) for p in metadata})
            peer.close()
            all_frames.extend(peer.frames)
            all_errors.extend(peer.errors)
            start()
            for d in roots.values():
                peer.result('session/load' if version == 1 else 'session/resume', {
                    'sessionId': d['sid'], 'cwd': str(d['a']), 'mcpServers': []})
                before = len(captures)
                response = prompt(d['sid'], [{'type': 'text', 'text': 'RELOAD_PROBE_' + d['marker']}])
                check('result' in response and len(captures) > before, response)
                systems = text([m for m in captures[-1]['messages'] if m['role'] in ('system', 'developer')])
                normalized = systems.replace('\\\\', '/').replace('\\', '/').lower()
                check(d['a'].as_posix().lower() in normalized and d['b'].as_posix().lower() in normalized,
                    ('reload lost additional directory or cwd', systems))
            return {'restart_metadata_preserved': True}
        case('additional-directories-restart-metadata', 'positive', reload_roots)
        check(not http_requests, http_requests)
        return {'version': version, 'cases': cases, 'model_calls': len(captures),
            'positive_passed': sum(c['result'] == 'passed' and c['kind'] == 'positive' for c in cases),
            'negative_passed': sum(c['result'] == 'passed' and c['kind'] == 'negative' for c in cases),
            'failed': sum(c['result'] == 'failed' for c in cases),
            'result': 'passed' if all(c['result'] == 'passed' for c in cases) else 'failed'}
    finally:
        try:
            if peer:
                peer.close()
        finally:
            server.shutdown()
            server.server_close()
            dump(root / 'model-inputs.json', captures)
            dump(root / 'model-errors.json', model_errors)
            dump(root / 'resource-http-requests.json', http_requests)
            dump(root / 'acp-frames.json', locals().get('all_frames', []) + (peer.frames if peer else []))
            (root / 'acp-stderr.log').write_text(''.join(locals().get('all_errors', []) + (peer.errors if peer else [])), encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('native', 'bridge', 'artifacts'):
        parser.add_argument('--' + name, required=True, type=Path)
    for name in ('native', 'bridge'):
        parser.add_argument('--expect-' + name + '-sha256')
    args = parser.parse_args()
    for name in ('native', 'bridge', 'artifacts'):
        path = getattr(args, name)
        check(path.is_absolute() and path.drive.lower() == 'c:', ('required absolute C: path', path))
        setattr(args, name, path.resolve())
    check(not args.artifacts.exists(), 'artifacts must be a new unique directory (never overwrite prior evidence)')
    args.artifacts.mkdir(parents=True)
    report = {'result': 'failed', 'command': subprocess.list2cmdline([sys.executable] + sys.argv),
        'revision': REVISION, 'binaries': {}, 'runs': [],
        'limits': ['Only local controlled OpenAI-completions provider, selected text/image model.',
                   'No external AI; no claim of complete multimodal support.',
                   'Additional directories are not asserted to sandbox native full filesystem access.']}
    try:
        for name in ('native', 'bridge'):
            path = getattr(args, name)
            sha = hashlib.sha256(path.read_bytes()).hexdigest()
            report['binaries'][name] = {'path': str(path), 'sha256': sha, 'size': path.stat().st_size}
            expected = getattr(args, 'expect_' + name + '_sha256')
            check(expected is None or sha == expected.lower(), ('binary identity pin mismatch', name, sha, expected))
        for version in (1, 2):
            try:
                report['runs'].append(run(args, args.artifacts / ('v' + str(version)), version))
            except Exception:
                report['runs'].append({'version': version, 'result': 'failed', 'error': traceback.format_exc()})
            dump(args.artifacts / 'result.json', report)
        report['result'] = 'passed' if all(r['result'] == 'passed' for r in report['runs']) else 'failed'
    except Exception:
        report['error'] = traceback.format_exc()
    dump(args.artifacts / 'result.json', report)
    print(json.dumps(report, indent=2))
    return int(report['result'] != 'passed')


if __name__ == '__main__':
    sys.dont_write_bytecode = True
    raise SystemExit(main())
