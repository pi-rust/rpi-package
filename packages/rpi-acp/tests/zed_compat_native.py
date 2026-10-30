#!/usr/bin/env python3
"""Frozen real-native Zed compatibility acceptance (stdlib; no external model).

Only child environment is allowlisted. RPI_EXTENSIONS_DIR deliberately absent.
RPI_ACP_NO_PERMISSION_GATE=1 is controlled tool-policy testing, NOT approval
acceptance. Refuses existing artifact roots; all failures and wire evidence stay.
"""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import queue
import re
import struct
import subprocess
import sys
import threading
import time
import traceback
import wave
import zlib
from http.server import BaseHTTPRequestHandler, HTTPServer

PINS = {'bridge': '12751b43a25028188319f3f148732e7f0777c6830818b8de478d8c83c18349e7',
        'native': 'b1b371966a3dfe6227e863e8a90655059dea86218d49d8bda4ef9564eb241888'}
SUMMARY = 'LOCAL_COMPACTION_SUMMARY_01: user history retained; no pending work.'


def require(ok, detail):
    if not ok:
        raise AssertionError(detail)


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')


def encoded(value):
    return json.dumps(value, ensure_ascii=False)


def media():
    def chunk(t, d):
        return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0))
    png += chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\xff')) + chunk(b'IEND', b'')
    out = io.BytesIO()
    with wave.open(out, 'wb') as f:
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(8000)
        f.writeframes(b'\x00\x00' * 80)
    return png, out.getvalue()


class Fixture:
    """Single non-daemon HTTP owner thread, socket deadlines, bounded close/join."""
    def __init__(self, root):
        self.root, self.requests, self.errors = root, [], []
        self.action = None
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(8)

            def log_message(self, *args):
                pass

            def do_GET(self):
                fixture.errors.append('Unexpected URI fetch: ' + self.path)
                self.send_error(404)

            def do_POST(self):
                try:
                    require(self.path == '/v1/chat/completions', self.path)
                    size = int(self.headers['Content-Length'])
                    require(0 < size < 8 * 1024 * 1024, size)
                    body = json.loads(self.rfile.read(size))
                    fixture.requests.append({'path': self.path, 'body': body, 'action': fixture.action})
                    save(root / 'model-requests.json', fixture.requests)
                    action = fixture.action
                    summarizing = any(m.get('role') == 'system' and 'context summarization assistant' in str(m.get('content')) for m in body['messages'])
                    delta, finish = {'content': SUMMARY if summarizing or action == 'compact' else 'LOCAL_NATIVE_OK'}, 'stop'
                    if isinstance(action, dict):
                        ident = action['id']
                        results = [m for m in body['messages'] if m.get('tool_call_id') == ident]
                        if not results:
                            delta = {'tool_calls': [{'index': 0, 'id': ident, 'type': 'function', 'function': {
                                'name': 'write', 'arguments': encoded({'path': action['path'], 'content': action['content']})}}]}
                            finish = 'tool_calls'
                    if body.get('stream'):
                        events = [{'choices': [{'index': 0, 'delta': {'role': 'assistant', **delta}, 'finish_reason': None}]},
                                  {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}],
                                   'usage': {'prompt_tokens': 100, 'completion_tokens': 20, 'total_tokens': 120}}]
                        data = (''.join('data: ' + encoded(e) + '\n\n' for e in events) + 'data: [DONE]\n\n').encode()
                        content_type = 'text/event-stream'
                    else:
                        data = encoded({'id': 'local-response', 'object': 'chat.completion', 'model': 'fixture',
                            'choices': [{'index': 0, 'message': {'role': 'assistant', **delta}, 'finish_reason': finish}],
                            'usage': {'prompt_tokens': 100, 'completion_tokens': 20, 'total_tokens': 120}}).encode()
                        content_type = 'application/json'
                    self.send_response(200)
                    self.send_header('Content-Type', content_type)
                    self.send_header('Content-Length', str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except Exception:
                    fixture.errors.append(traceback.format_exc())
                    self.send_error(500, 'fixture failure')
        self.server = HTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.1), name='local-model', daemon=False)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(10)
        require(not self.thread.is_alive(), 'HTTP owner thread did not join')
        save(self.root / 'model-errors.json', self.errors)
        save(self.root / 'model-requests.json', self.requests)


def ownership(candidate, parent, root_created, native):
    """Fail closed: a PPID match alone is never ownership or exit evidence."""
    if 'query_error' in candidate:
        return 'unverified', 'process-query-failed'
    if os.path.normcase(candidate['image']) != os.path.normcase(str(native)):
        return 'excluded', 'image-mismatch'
    if candidate['created'] < root_created:
        return 'excluded', 'created-before-owner'
    if parent is None or candidate['created'] <= parent['created']:
        return 'excluded', 'invalid-parent-chain'
    return 'owned', 'image-and-increasing-creation-chain'


def ownership_self_test():
    native, root = 'C:/frozen/rpi.exe', {'created': 100}
    tests = [
        ('owned', {'image': native, 'created': 102}, root, 'owned'),
        ('older-than-root', {'image': native, 'created': 99}, root, 'excluded'),
        ('wrong-image', {'image': 'C:/other/rpi.exe', 'created': 102}, root, 'excluded'),
        ('query-access-denied', {'query_error': 5}, root, 'unverified'),
        ('parent-newer-than-child', {'image': native, 'created': 102}, {'created': 103}, 'excluded'),
        ('equal-parent-creation', {'image': native, 'created': 102}, {'created': 102}, 'excluded'),
        ('unverified-parent', {'image': native, 'created': 102}, None, 'excluded'),
    ]
    results = []
    for name, candidate, parent, expected in tests:
        status, reason = ownership(candidate, parent, root['created'], native)
        require(status == expected, (name, status, expected))
        # Only owned candidates are retained for wait/termination. In particular,
        # access denied is NOT a successful wait and supplies no exit code.
        require(status != 'owned' or 'query_error' not in candidate, name)
        results.append({'name': name, 'status': status, 'reason': reason})
    return results


class WindowsProcesses:
    """Owned process handles; no PID reopen after EOF, no tree/name killing."""
    def __init__(self):
        import ctypes
        from ctypes import wintypes as w
        self.c, self.w = ctypes, w
        self.k = ctypes.WinDLL('kernel32', use_last_error=True)
        signatures = {
            'CreateToolhelp32Snapshot': ([w.DWORD, w.DWORD], w.HANDLE),
            'Process32FirstW': ([w.HANDLE, ctypes.c_void_p], w.BOOL),
            'Process32NextW': ([w.HANDLE, ctypes.c_void_p], w.BOOL),
            'OpenProcess': ([w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            'QueryFullProcessImageNameW': ([w.HANDLE, w.DWORD, w.LPWSTR, ctypes.POINTER(w.DWORD)], w.BOOL),
            'GetProcessTimes': ([w.HANDLE] + [ctypes.POINTER(w.FILETIME)] * 4, w.BOOL),
            'WaitForSingleObject': ([w.HANDLE, w.DWORD], w.DWORD),
            'GetExitCodeProcess': ([w.HANDLE, ctypes.POINTER(w.DWORD)], w.BOOL),
            'TerminateProcess': ([w.HANDLE, w.UINT], w.BOOL),
            'CloseHandle': ([w.HANDLE], w.BOOL),
        }
        for name, (args, result) in signatures.items():
            fn = getattr(self.k, name)
            fn.argtypes, fn.restype = args, result

    def proof(self, handle):
        c, w = self.c, self.w
        size = w.DWORD(32768)
        image = c.create_unicode_buffer(size.value)
        if not self.k.QueryFullProcessImageNameW(handle, 0, image, c.byref(size)):
            raise c.WinError(c.get_last_error())
        times = [w.FILETIME() for _ in range(4)]
        if not self.k.GetProcessTimes(handle, *(c.byref(t) for t in times)):
            raise c.WinError(c.get_last_error())
        created = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        return {'image': image.value, 'created': created}

    def snapshot(self):
        c, w = self.c, self.w
        class Entry(c.Structure):
            _fields_ = [('size', w.DWORD), ('usage', w.DWORD), ('pid', w.DWORD),
                ('heap', c.c_size_t), ('module', w.DWORD), ('threads', w.DWORD),
                ('parent', w.DWORD), ('priority', w.LONG), ('flags', w.DWORD), ('exe', w.WCHAR * 260)]
        handle = self.k.CreateToolhelp32Snapshot(2, 0)
        if handle == c.c_void_p(-1).value:
            raise c.WinError(c.get_last_error())
        pairs, entry = [], Entry()
        entry.size = c.sizeof(entry)
        try:
            more = self.k.Process32FirstW(handle, c.byref(entry))
            while more:
                pairs.append((entry.pid, entry.parent))
                more = self.k.Process32NextW(handle, c.byref(entry))
            require(c.get_last_error() == 18, ('process enumeration failed', c.get_last_error()))
            return pairs
        finally:
            self.k.CloseHandle(handle)

    def retain_native(self, pid, root_proof, native, records, held):
        pairs, seen, parents = self.snapshot(), {pid}, {pid: root_proof}
        pending = [pid]
        while pending:
            parent_pid = pending.pop(0)
            for candidate_pid, ppid in pairs:
                if ppid != parent_pid or candidate_pid in seen:
                    continue
                seen.add(candidate_pid)
                pending.append(candidate_pid)
                record = {'pid': candidate_pid, 'parent_pid': ppid,
                    'root_created': root_proof['created']}
                # SYNCHRONIZE | QUERY_LIMITED_INFORMATION, plus TERMINATE for
                # timeout fallback on this SAME retained handle, never a PID.
                handle = self.k.OpenProcess(0x100000 | 0x1000 | 0x1, False, candidate_pid)
                try:
                    if not handle:
                        record['query_error'] = self.c.get_last_error()
                    else:
                        try:
                            record.update(self.proof(handle))
                        except OSError as error:
                            record['query_error'] = error.winerror
                    parent = parents.get(ppid)
                    record['status'], record['reason'] = ownership(record, parent, root_proof['created'], native)
                    if parent is not None:
                        record['parent_created'] = parent['created']
                    records.append(record)
                    if record['status'] == 'owned':
                        parents[candidate_pid] = record
                        held.append((handle, record))
                        handle = None
                finally:
                    if handle:
                        self.k.CloseHandle(handle)

    def wait_exit(self, handle, milliseconds):
        result = self.k.WaitForSingleObject(handle, milliseconds)
        evidence = {'wait_result': result, 'exited': result == 0}
        if result == 0:
            code = self.w.DWORD()
            if not self.k.GetExitCodeProcess(handle, self.c.byref(code)):
                raise self.c.WinError(self.c.get_last_error())
            evidence['exit_code'] = code.value
        elif result != 258:
            evidence['wait_error'] = self.c.get_last_error()
        return evidence


class Peer:
    def __init__(self, args, root, env, index, command=None):
        self.root, self.frames, self.errors, self.seq = root, [], [], 0
        self.q, self.closed = queue.Queue(), False
        self.prefix = 'bridge-' + str(index)
        self.frame_file = (root / (self.prefix + '-frames.jsonl')).open('x', encoding='utf-8')
        self.p = subprocess.Popen(command or [str(args.bridge)], cwd=root / 'launch', env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding='utf-8', bufsize=1)
        self.lifecycle = {'pid': self.p.pid, 'forced': False, 'native_fallbacks': []}
        self.windows = WindowsProcesses() if os.name == 'nt' else None
        self.raw_native = command is not None
        if self.windows:
            try:
                # Popen's original handle is authoritative even if its PID is reused.
                self.lifecycle['root_proof'] = self.windows.proof(int(self.p._handle))
            except Exception:
                self.p.kill()
                self.p.wait(5)
                for stream in (self.p.stdin, self.p.stdout, self.p.stderr):
                    stream.close()
                self.frame_file.close()
                raise
        self.native = args.native
        self.lock = threading.Lock()

        def stdout():
            try:
                for line in self.p.stdout:
                    try:
                        value = json.loads(line)
                    except Exception:
                        value = {'invalid_stdout': line}
                    self.record('receive', value)
                    self.q.put(value)
            finally:
                self.q.put({'eof': True})

        def stderr():
            with (root / (self.prefix + '-stderr.log')).open('x', encoding='utf-8') as f:
                for line in self.p.stderr:
                    self.errors.append(line)
                    f.write(line)
                    f.flush()
        self.threads = [threading.Thread(target=stdout, name=self.prefix + '-stdout', daemon=False),
                        threading.Thread(target=stderr, name=self.prefix + '-stderr', daemon=False)]
        for thread in self.threads:
            thread.start()

    def record(self, direction, value):
        with self.lock:
            self.frame_file.write(encoded({'direction': direction, 'frame': value}) + '\n')
            self.frame_file.flush()

    def receive(self, deadline):
        frame = self.q.get(timeout=max(.01, deadline - time.monotonic()))
        self.frames.append(frame)
        require(not frame.get('eof') and 'invalid_stdout' not in frame, frame)
        require(not ('id' in frame and 'method' in frame), ('unexpected client interaction in controlled no-approval mode', frame))
        return frame

    def request(self, method, params, timeout=60):
        self.seq += 1
        ident, start = self.seq, len(self.frames)
        frame = {'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params}
        self.record('send', frame)
        self.p.stdin.write(encoded(frame) + '\n')
        self.p.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            frame = self.receive(deadline)
            if frame.get('id') == ident and 'method' not in frame:
                return frame, self.frames[start:]

    def result(self, method, params, timeout=60):
        frame, frames = self.request(method, params, timeout)
        require('result' in frame, frame)
        if method == 'initialize':
            self.protocol_version = frame['result']['protocolVersion']
        if method in ('session/new', 'session/load', 'session/resume') and self.protocol_version == 1:
            sid = frame['result'].get('sessionId', params.get('sessionId'))
            require(not any(u['sessionUpdate'] == 'available_commands_update' for u in updates(frames, sid)),
                ('command inventory arrived before session registration', method))
            # Native under test supports compact. It is now a post-response
            # notification; keep the strict inventory assertion at this boundary.
            frames = list(frames)
            deadline = time.monotonic() + timeout
            while not any(u['sessionUpdate'] == 'available_commands_update' for u in updates(frames, sid)):
                frames.append(self.receive(deadline))
        return frame['result'], frames

    def close(self):
        if self.closed:
            return
        self.closed = True
        held, records, cleanup_errors = [], [], []
        self.lifecycle['native_candidates_before_eof'] = records
        try:
            if self.windows and not self.raw_native:
                try:
                    self.windows.retain_native(self.p.pid, self.lifecycle['root_proof'], self.native, records, held)
                except Exception:
                    cleanup_errors.append(traceback.format_exc())
            self.lifecycle['owned_descendants_before_eof'] = [r['pid'] for _, r in held]
            if not self.p.stdin.closed:
                self.p.stdin.close()
            try:
                self.p.wait(20)
            except subprocess.TimeoutExpired:
                self.lifecycle['forced'] = True
                # Only the Popen-owned root; never taskkill or a PPID tree.
                self.p.kill()
                self.p.wait(5)
            if self.windows:
                self.lifecycle['root_exit'] = self.windows.wait_exit(int(self.p._handle), 0)
                for handle, record in held:
                    record.update(self.windows.wait_exit(handle, 5000))
                    if record['wait_result'] == 258:
                        self.lifecycle['forced'] = True
                        self.lifecycle['native_fallbacks'].append(record['pid'])
                        record['forced'] = True
                        record['terminate_ok'] = bool(self.windows.k.TerminateProcess(handle, 99))
                        if not record['terminate_ok']:
                            record['terminate_error'] = self.windows.c.get_last_error()
                        record.update(self.windows.wait_exit(handle, 5000))
                    # Preserve the original contract: root exit must be zero;
                    # bridge-managed natives must exit, with their actual code
                    # recorded (the bridge may terminate them during teardown).
                    if not record['exited']:
                        cleanup_errors.append({'native_exit_failed': dict(record)})
            for thread in self.threads:
                thread.join(10)
            self.lifecycle.update(exit_code=self.p.returncode, readers_joined=all(not t.is_alive() for t in self.threads),
                cleanup_errors=cleanup_errors)
            require(self.lifecycle['readers_joined'], 'bridge reader did not join')
            for stream in (self.p.stdin, self.p.stdout, self.p.stderr):
                if not stream.closed:
                    stream.close()
            require(not cleanup_errors and not self.lifecycle['forced'] and not self.lifecycle['native_fallbacks'] and self.p.returncode == 0, self.lifecycle)
        finally:
            for handle, record in held:
                record['handle_closed'] = bool(self.windows.k.CloseHandle(handle))
            self.frame_file.close()
            save(self.root / (self.prefix + '-lifecycle.json'), self.lifecycle)


def updates(frames, sid):
    return [f['params']['update'] for f in frames if f.get('method') == 'session/update' and f['params']['sessionId'] == sid]


def mode(value, version, expected):
    options = [o for o in value.get('configOptions', []) if o.get('id' if version == 1 else 'configId') == 'mode']
    require(len(options) == 1, ('missing mode config', value))
    option = options[0]
    require(option['type'] == 'select' and option['currentValue'] == expected, option)
    require({'default', 'plan'} <= {o['value'] for o in option['options']}, option)
    if version == 1 and 'modes' in value:
        require(value['modes']['currentModeId'] == expected, value)
    return option


def commands(frames, sid, version):
    notices = [u for u in updates(frames, sid) if u['sessionUpdate'] == 'available_commands_update']
    names = [c['name'].lstrip('/') for u in notices for c in u['availableCommands']]
    require(('compact' in names) if version == 1 else ('compact' not in names), ('compact advertisement', version, notices))


def run(args, root, version):
    root.mkdir()
    for name in ('launch', 'agent', 'home', 'appdata', 'localappdata', 'temp'):
        (root / name).mkdir()
    fixture = Fixture(root)
    peers, cases = [], []
    peer = None
    # Positive allowlist, never copy PATH/credentials/proxies or arbitrary RPI_*.
    basic = {'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'SYSTEMDRIVE', 'OS', 'PROCESSOR_ARCHITECTURE', 'NUMBER_OF_PROCESSORS', 'PATHEXT'}
    env = {k: v for k, v in os.environ.items() if k.upper() in basic}
    env.update(HOME=str(root / 'home'), USERPROFILE=str(root / 'home'), APPDATA=str(root / 'appdata'),
        LOCALAPPDATA=str(root / 'localappdata'), TEMP=str(root / 'temp'), TMP=str(root / 'temp'),
        RPI_BIN=str(args.native), RPI_ACP_NO_AUTO_REGISTER_ZED='1', RPI_OFFLINE='1',
        RPI_CODING_AGENT_DIR=str(root / 'agent'), RPI_ACP_SESSION_DIR=str(root / 'sessions'),
        RPI_ACP_NO_PERMISSION_GATE='1')
    save(root / 'environment.json', env)
    save(root / 'agent/auth.json', {})
    save(root / 'agent/models.json', {'providers': {'zed-local': {
        'api': 'openai-completions', 'baseUrl': f'http://127.0.0.1:{fixture.server.server_port}/v1', 'apiKey': 'local-only',
        'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False, 'input': ['text', 'image'],
                    'contextWindow': 100000, 'maxTokens': 1024}]}}})
    save(root / 'agent/settings.json', {'defaultProvider': 'zed-local', 'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'})

    def case(name, fn):
        start, before = time.monotonic(), len(fixture.requests)
        try:
            detail = fn()
            cases.append({'name': name, 'result': 'passed', 'detail': detail})
        except Exception:
            cases.append({'name': name, 'result': 'failed', 'error': traceback.format_exc()})
        cases[-1].update(seconds=round(time.monotonic() - start, 3), model_start=before, model_end=len(fixture.requests))
        save(root / 'cases.json', cases)

    def start():
        nonlocal peer
        peer = Peer(args, root, env, len(peers) + 1)
        peers.append(peer)
        caps = {'session': {'compaction': {}}}
        params = {'protocolVersion': version, **({'clientCapabilities': caps} if version == 1 else {
            'info': {'name': 'zed-native-acceptance', 'version': '1'}, 'capabilities': caps})}
        result, _ = peer.result('initialize', params)
        require(result['protocolVersion'] == version, result)
        return result

    def new():
        return peer.result('session/new', {'cwd': str(root / 'launch'), 'mcpServers': []})

    def set_mode(sid, selected, legacy=False):
        result, frames = peer.result('session/set_mode' if legacy else 'session/set_config_option',
            {'sessionId': sid, **({'modeId': selected} if legacy else {'configId': 'mode', 'type': 'select', 'value': selected})})
        if not legacy:
            mode(result, version, selected)
        events = updates(frames, sid)
        configs = [u for u in events if u['sessionUpdate'] == 'config_option_update']
        require(len(configs) == 1, events)
        mode(configs[0], version, selected)
        if version == 1:
            current = [u for u in events if u['sessionUpdate'] == 'current_mode_update']
            require(len(current) == 1 and current[0]['currentModeId'] == selected, events)
        return result

    def prompt(sid, blocks):
        since = len(peer.frames)
        result, _ = peer.result('session/prompt', {'sessionId': sid, 'prompt': blocks})
        if version == 2:
            deadline = time.monotonic() + 60
            while not any(u.get('state') == 'idle' for u in updates(peer.frames[since:], sid)):
                peer.receive(deadline)
            idle = [u for u in updates(peer.frames[since:], sid) if u.get('state') == 'idle'][-1]
            require(idle.get('stopReason') == 'end_turn', idle)
        else:
            require(result.get('stopReason') == 'end_turn', result)
        return result, peer.frames[since:]

    def restore(sid):
        result, frames = peer.result('session/load' if version == 1 else 'session/resume', {
            'sessionId': sid, 'cwd': str(root / 'launch'), 'mcpServers': [],
            **({'replayFrom': {'type': 'start'}} if version == 2 else {})})
        commands(frames, sid, version)
        return result, frames

    try:
        save(root / 'initialize.json', start())
        created, frames = new()
        sid = created['sessionId']
        case('new-mode-select-and-compact-advertisement', lambda: (mode(created, version, 'default'), commands(frames, sid, version)))
        case('config-plan-native-ack-and-synchronized-updates', lambda: set_mode(sid, 'plan'))

        def invalid_modes():
            before = len(fixture.requests)
            for value in ('unknown-mode-01', 7, None, {}, True):
                frame, _ = peer.request('session/set_config_option', {'sessionId': sid, 'configId': 'mode', 'type': 'select', 'value': value})
                require(frame.get('error', {}).get('code') == -32602, frame)
            if version == 1:
                for value in ('unknown-mode-01', 7):
                    frame, _ = peer.request('session/set_mode', {'sessionId': sid, 'modeId': value})
                    require(frame.get('error', {}).get('code') == -32602, frame)
            require(len(fixture.requests) == before, 'invalid mode invoked model')
        case('unknown-and-nonstring-modes-rejected', invalid_modes)

        def policy(selected):
            set_mode(sid, selected)
            target = root / 'launch' / (selected + '-controlled.txt')
            action = {'id': selected + '-write', 'path': str(target), 'content': 'REAL_NATIVE_WRITE_01'}
            fixture.action = action
            before = len(fixture.requests)
            try:
                _, frames = prompt(sid, [{'type': 'text', 'text': 'POLICY_' + selected + ': attempt controlled write'}])
            finally:
                fixture.action = None
            calls = fixture.requests[before:]
            require(len(calls) >= 2, ('model write attempt/result missing', calls))
            for call in calls:
                names = {t['function']['name'] for t in call['body'].get('tools', [])}
                if selected == 'plan':
                    require(not names.intersection({'write', 'edit', 'bash'}), ('unsafe plan schema', names))
                else:
                    require('write' in names, ('default write tool absent', names))
            tool_results = [m for c in calls for m in c['body']['messages'] if m.get('tool_call_id') == action['id']]
            require(tool_results, 'native write result never reached model')
            if selected == 'plan':
                require(not target.exists(), 'Plan model write reached filesystem')
                require(any(u.get('status') == 'failed' for u in updates(frames, sid)), ('Plan failure not reported', frames))
            else:
                require(target.read_text(encoding='utf-8') == action['content'], 'default native write missing')
            return {'target': str(target), 'model_calls': len(calls), 'tool_results': tool_results}
        case('plan-schema-and-real-native-write-denial', lambda: policy('plan'))
        if version == 1:
            case('legacy-set-mode-default-synchronized', lambda: set_mode(sid, 'default', True))
        case('default-same-write-tool-real-filesystem', lambda: policy('default'))

        def close_reopen():
            set_mode(sid, 'plan')
            peer.result('session/close', {'sessionId': sid})
            result, _ = restore(sid)
            mode(result, version, 'plan')
            return {'sessionId': sid, 'mode': 'plan'}
        case('mode-survives-close-reopen', close_reopen)

        png, wav = media()
        (root / 'input.png').write_bytes(png)
        (root / 'input.wav').write_bytes(wav)
        uri = (root / 'launch').as_uri() + '/full-directory/?q=keep%20uri#fragment'
        image = {'type': 'image', 'data': base64.b64encode(png).decode(), 'mimeType': 'image/png'}
        audio = {'type': 'audio', 'data': base64.b64encode(wav).decode(), 'mimeType': 'audio/wav'}
        blocks = [{'type': 'text', 'text': 'MEDIA_ORDER_BEFORE'},
                  {'type': 'resource_link', 'name': 'short-colliding-name', 'uri': uri}, image,
                  {'type': 'text', 'text': 'MEDIA_ORDER_MIDDLE'}, audio, {'type': 'text', 'text': 'MEDIA_ORDER_AFTER'}]
        media_created, _ = new()
        media_sid = media_created['sessionId']
        baseline = {}

        def incoming_media():
            before = len(fixture.requests)
            result, frames = prompt(media_sid, blocks)
            require(len(fixture.requests) == before + 1, 'mixed input did not make exactly one model call')
            user = [m for m in fixture.requests[-1]['body']['messages'] if m['role'] == 'user'][-1]
            parts = user['content']
            require([p['type'] for p in parts] == ['text', 'image_url', 'text', 'input_audio', 'text'], ('provider order', user))
            require(uri in parts[0]['text'] and 'MEDIA_ORDER_BEFORE' in parts[0]['text'], ('full directory URI lost', user))
            require(parts[1]['image_url']['url'] == 'data:image/png;base64,' + image['data'], ('provider image bytes/MIME lost', user))
            require(parts[3]['input_audio'] == {'data': audio['data'], 'format': 'wav'}, ('provider audio bytes/format lost', user))
            require(parts[2]['text'] == 'MEDIA_ORDER_MIDDLE' and parts[4]['text'] == 'MEDIA_ORDER_AFTER', user)
            baseline['prompt_result'] = result
            baseline['live_updates'] = updates(frames, media_sid)
            save(root / 'media-live.json', baseline)
            return {'sessionId': media_sid, 'provider_user': user}
        case('mixed-full-directory-uri-image-and-audio-to-real-model', incoming_media)

        def capture_history():
            value, _ = peer.result('_rpi/history', {'sessionId': media_sid, 'limit': 128})
            require(not value.get('hasMore'), 'unexpected unbounded media history')
            us = [u for u in value['updates'] if u['sessionUpdate'] == 'user_message_chunk']
            require([u['content']['type'] for u in us] == ['text', 'image', 'text', 'audio', 'text'], us)
            require(us[1]['content'] == image and us[3]['content'] == audio, ('pre-restart media missing', us))
            require(uri in us[0]['content']['text'], ('pre-restart URI lost', us))
            ids = {u['messageId'] for u in us}
            require(len(ids) == 1 and next(iter(ids)), us)
            entries = [json.loads(line) for path in (root / 'sessions' / media_sid).rglob('*.jsonl')
                for line in path.read_text(encoding='utf-8').splitlines()]
            original = [e for e in entries if e.get('kind') == 'entry' and e.get('type') == 'message'
                and e.get('message', {}).get('role') == 'user']
            require(len(original) == 1, ('native original user entry missing', original))
            require(next(iter(ids)) == 'history:' + original[0]['id'], ('native original ID lost', original, us))
            require(original[0]['message']['content'] == [u['content'] for u in us], ('native media/order diverged', original, us))
            baseline['native_original_entry'] = original[0]
            if version == 2:
                require(baseline['prompt_result']['messageId'] == next(iter(ids)), ('live user ID changed', baseline, us))
            baseline['history'] = us
            save(root / 'media-before-restart.json', baseline)
            return us
        case('original-history-media-and-message-id', capture_history)

        if version == 1:
            def compact_empty():
                empty, advertised = new()
                empty_sid = empty['sessionId']
                commands(advertised, empty_sid, version)
                before, started = len(fixture.requests), time.monotonic()
                reply, frames = peer.request('session/prompt', {
                    'sessionId': empty_sid, 'prompt': [{'type': 'text', 'text': '/compact'}]}, timeout=5)
                require(time.monotonic() - started < 3, 'declined compaction waited for timeout')
                require(reply.get('error', {}).get('message') == 'Nothing to compact yet.', reply)
                events = [u for u in updates(frames, empty_sid) if u['sessionUpdate'] == 'compaction_update']
                require([u['status'] for u in events] == ['in_progress', 'failed'], events)
                require(len(fixture.requests) == before, 'empty compaction invoked model')
                # Native settled as declined; it must not be killed or remain busy.
                prompt(empty_sid, [{'type': 'text', 'text': 'AFTER_DECLINED_COMPACTION'}])
                require(len(fixture.requests) == before + 1, 'declined compaction broke next native run')
                return {'reply': reply, 'events': events, 'native_followup_ok': True}
            case('v1-empty-compact-declines-promptly-without-killing-native', compact_empty)

            def compact():
                compact_created, compact_frames = new()
                csid = compact_created['sessionId']
                commands(compact_frames, csid, version)
                for n in range(3):
                    prompt(csid, [{'type': 'text', 'text': 'HISTORY_FOR_COMPACTION_' + str(n) + ' ' + ('bounded history. ' * 80)}])
                before = len(fixture.requests)
                since = len(peer.frames)
                fixture.action = 'compact'
                try:
                    reply, frames = peer.request('session/prompt', {'sessionId': csid, 'prompt': [{'type': 'text', 'text': '/compact'}]})
                    # A premature adapter error must not race fixture teardown:
                    # wait for the native-owned persisted compaction evidence.
                    deadline = time.monotonic() + 10
                    native_entries = []
                    while time.monotonic() < deadline:
                        native_entries = [json.loads(line) for path in (root / 'sessions' / csid).rglob('*.jsonl')
                            for line in path.read_text(encoding='utf-8').splitlines()
                            if '"type":"compaction"' in line]
                        if any(e.get('kind') == 'entry' for e in native_entries):
                            break
                        time.sleep(.05)
                finally:
                    fixture.action = None
                calls = fixture.requests[before:]
                events = [u for u in updates(peer.frames[since:], csid) if u['sessionUpdate'] == 'compaction_update']
                save(root / 'compaction-evidence.json', {'reply': reply, 'events': events,
                    'model_requests': calls, 'native_compaction_entries': native_entries})
                require('result' in reply, ('ACP rejected actual compaction', reply))
                require(reply['result'].get('stopReason') == 'end_turn', reply)
                require(calls, 'compact made no real summarization request')
                require(all(not any(m.get('role') == 'user' and m.get('content') == '/compact' for m in c['body']['messages']) for c in calls), 'slash command became ordinary user message')
                events = [u for u in updates(frames, csid) if u['sessionUpdate'] == 'compaction_update']
                require(any(u['status'] == 'in_progress' for u in events), events)
                done = [u for u in events if u['status'] == 'completed']
                persisted = [e for e in native_entries if e.get('kind') == 'entry' and e.get('type') == 'compaction']
                require(len(persisted) == 1 and persisted[0]['summary'] == SUMMARY,
                    ('canonical model summary missing', persisted))
                # Native compaction_json intentionally sends a status line, not
                # the model-authored persisted summary (modes.rs contract).
                entry = persisted[0]
                expected_status = 'Conversation compacted · {} tokens before, {} messages kept.'.format(
                    entry['tokensBefore'], len(entry['retainedTail']))
                require(len(done) == 1 and done[0].get('summary') == [{'type': 'text', 'text': expected_status}],
                    ('native status projection changed', events))
                started = [u for u in events if u['status'] == 'in_progress']
                require(len(started) == 1 and started[0]['compactionId'] == done[0]['compactionId'],
                    ('compaction identity changed', events))
                history, _ = peer.result('_rpi/history', {'sessionId': csid, 'limit': 128})
                require(not any(u.get('content', {}).get('text') == '/compact' for u in history['updates']), 'ordinary slash message persisted')
                save(root / 'compaction-evidence.json', {'reply': reply, 'events': events, 'history': history,
                    'model_requests': calls, 'native_compaction_entries': native_entries})
                return {'summary': SUMMARY, 'model_calls': len(calls)}
            case('v1-slash-compact-real-native-summarization', compact)

            def native_compact_wire_diagnostic():
                raw = Peer(args, root, env, 'raw-native', command=[str(args.native), '--mode', 'rpc',
                    '--session-dir', str(root / 'direct-native')])
                peers.append(raw)
                def command(ident, kind, **params):
                    value = {'id': ident, 'type': kind, **params}
                    raw.record('send', value)
                    raw.p.stdin.write(encoded(value) + '\n')
                    raw.p.stdin.flush()
                    deadline = time.monotonic() + 15
                    while True:
                        frame = raw.receive(deadline)
                        if frame.get('id') == ident and frame.get('type') == 'response':
                            return frame
                deadline = time.monotonic() + 15
                while not any(f.get('type') == 'ready' for f in raw.frames):
                    raw.receive(deadline)
                command('p1', 'prompt', message='DIRECT_NATIVE_COMPACT_HISTORY ' + ('history ' * 100))
                deadline = time.monotonic() + 15
                while not any(f.get('type') == 'agent_settled' for f in raw.frames):
                    raw.receive(deadline)
                reply = command('c1', 'compact')
                deadline = time.monotonic() + 15
                while not any(f.get('type') == 'agent_settled' and f.get('outcome') == 'compacted' for f in raw.frames):
                    raw.receive(deadline)
                save(root / 'native-compact-wire.json', {'response': reply, 'frames': raw.frames})
                require(reply.get('success') is True, reply)
                return {'actual_compact_reply': reply}
            case('diagnostic-real-native-compact-wire-contract', native_compact_wire_diagnostic)

        def restart():
            peer.close()
            start()
            result, _ = restore(sid)
            mode(result, version, 'plan')
            return {'sessionId': sid, 'mode': 'plan', 'new_bridge_pid': peer.p.pid}
        case('whole-bridge-restart-mode-persistence', restart)

        def replay_media():
            before = len(fixture.requests)
            _, frames = restore(media_sid)
            us = [u for u in updates(frames, media_sid) if u['sessionUpdate'] == 'user_message_chunk']
            require('history' in baseline, 'original history assertion failed; no valid baseline')
            require(us == baseline['history'], ('media blocks/order/data/MIME/original messageId changed', baseline['history'], us))
            require(len(fixture.requests) == before, 'replay invoked model')
            save(root / 'media-replayed.json', us)
            return {'originalMessageId': us[0]['messageId'], 'blockTypes': [u['content']['type'] for u in us], 'model_calls': 0}
        case('whole-bridge-restart-load-resume-start-exact-media-replay', replay_media)
    except Exception:
        cases.append({'name': 'setup-or-run', 'result': 'failed', 'error': traceback.format_exc()})
    finally:
        for owned in peers:
            case('owner-cleanup-' + owned.prefix, owned.close)
        case('fixture-cleanup', fixture.close)
        if fixture.errors:
            cases.append({'name': 'fixture-errors', 'result': 'failed', 'errors': fixture.errors})
        save(root / 'cases.json', cases)
    return {'version': version, 'passed': sum(c['result'] == 'passed' for c in cases),
        'failed': sum(c['result'] == 'failed' for c in cases), 'model_requests': len(fixture.requests), 'cases': cases}


def main():
    if sys.argv[1:] == ['--self-test']:
        print(encoded({'ownership_self_tests': ownership_self_test()}))
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('bridge', 'native', 'artifacts'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('bridge', 'native'):
        parser.add_argument('--expected-' + name + '-sha256', default=PINS[name])
    args = parser.parse_args()
    for name in ('bridge', 'native', 'artifacts'):
        path = getattr(args, name).resolve()
        require(path.is_absolute() and path.drive.lower() == 'c:', ('absolute C: path required', path))
        setattr(args, name, path)
    require(not args.artifacts.exists(), 'refusing existing artifact directory; never remove old evidence')
    args.artifacts.mkdir(parents=True)
    report = {'result': 'failed', 'command': subprocess.list2cmdline([sys.executable] + sys.argv),
        'binaries': {}, 'runs': [], 'ownership_self_tests': ownership_self_test(),
        'limits': ['Controlled RPI_ACP_NO_PERMISSION_GATE=1 policy test; NOT permission approval acceptance.',
            'Local HTTP fixture only, real native RPC (no mock), no live Zed GUI acceptance.',
            'No RPI_EXTENSIONS_DIR; explicit Windows environment allowlist; no inherited credentials or proxies.']}
    try:
        for name in ('bridge', 'native'):
            path = getattr(args, name)
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            expected = getattr(args, 'expected_' + name + '_sha256')
            require(re.fullmatch(r'[0-9a-f]{64}', expected), ('invalid explicit SHA256 pin', name))
            report['binaries'][name] = {'path': str(path), 'sha256': digest, 'expected': expected, 'bytes': path.stat().st_size}
            require(digest == expected, ('frozen binary mismatch', name, digest))
        for version in (1, 2):
            report['runs'].append(run(args, args.artifacts / ('v' + str(version)), version))
            save(args.artifacts / 'result.json', report)
        report['passed'] = sum(r['passed'] for r in report['runs'])
        report['failed'] = sum(r['failed'] for r in report['runs'])
        report['model_requests'] = sum(r['model_requests'] for r in report['runs'])
        report['result'] = 'passed' if report['failed'] == 0 else 'failed'
    except Exception:
        report['error'] = traceback.format_exc()
    save(args.artifacts / 'result.json', report)
    print(encoded(report))
    return int(report['result'] != 'passed')


if __name__ == '__main__':
    sys.dont_write_bytecode = True
    raise SystemExit(main())
