#!/usr/bin/env python3
"""Real native child/model acceptance; only the loopback HTTP provider is fake.

python subagents_native.py --bridge ACP_EXE --native RPI_EXE --plugin STUB_DLL \
    --artifacts EMPTY_DIRECTORY [--producer TOOL_NAME]

Build/freeze candidates separately. This script never builds, installs, modifies
native source, or supplies child events/IDs. It requires a plugin tool which
starts and polls a host ChildAgent; a plain echo-only plugin-stub is insufficient.
The producer must accept a prompt (or text/task) string; --producer-arguments
can provide its exact JSON arguments, with {prompt} substituted in string values.
All config, transcripts, captures and reports stay under --artifacts.

Coverage is model-only children. Child tools are deliberately blocked by native
v1 opt-in safety; child UI/permissions/tool execution/cancellation are NOT proven.
EOF checks cover process cleanup, not semantic child cancellation correctness.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from protocol_regression import Bridge

OPT_IN = 'rpi/unstable-session-rfds'
REVISION = '1142c83bbfa9407f9f264d1e290ffafcd54eb79e'
CHILD_PROMPT = 'NATIVE_CHILD_MODEL_ONLY: return CHILD_MODEL_RESPONSE without tools.'
CHILD_TEXT = 'CHILD_MODEL_RESPONSE'
ROOT_TEXT = 'ROOT_MODEL_COMPLETE'


def check(value, detail):
    if not value:
        raise AssertionError(detail)


def dump(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False), encoding='utf-8')


def strings(value):
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return '\n'.join(strings(v) for v in value.values())
    if isinstance(value, list):
        return '\n'.join(strings(v) for v in value)
    return ''


class Peer:
    """Real executable, bounded JSONL reads, bounded EOF, exact owned PID cleanup."""
    def __init__(self, command, env, root):
        self.frames = []
        # Capture at the actual pipe boundary, including frames consumed by
        # subclass receive methods and stdout drained during close. The shared
        # lock orders outgoing requests before any response they can trigger.
        self.conversation = []
        self.conversation_lock = threading.Lock()
        self.root = root
        self.q = queue.Queue()
        self.seq = 0
        self.stderr = (root / 'stderr.log').open('w', encoding='utf-8')
        self.p = subprocess.Popen(command, cwd=root / 'workspace', env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr,
            text=True, encoding='utf-8')
        self.handles = []

        def read():
            for line in self.p.stdout:
                try:
                    frame = json.loads(line)
                except ValueError:
                    frame = {'invalid_stdout': line}
                with self.conversation_lock:
                    self.conversation.append({'direction': 'agent_to_client', 'frame': frame})
                self.q.put(frame)
            self.q.put({'eof': True})
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def send(self, value):
        wire = json.dumps(value) + '\n'
        with self.conversation_lock:
            self.conversation.append({'direction': 'client_to_agent', 'frame': json.loads(wire)})
            self.p.stdin.write(wire)
            self.p.stdin.flush()

    def receive(self, deadline):
        frame = self.q.get(timeout=max(.01, deadline - time.monotonic()))
        self.frames.append(frame)
        check('invalid_stdout' not in frame and not frame.get('eof'), frame)
        # No child client UI/tool requests are expected in this model-only test.
        check(not ('method' in frame and 'id' in frame), ('unexpected client request', frame))
        return frame

    def request(self, method, params, timeout=45):
        self.seq += 1
        rid = self.seq
        self.send({'jsonrpc': '2.0', 'id': rid, 'method': method, 'params': params})
        deadline = time.monotonic() + timeout
        while True:
            frame = self.receive(deadline)
            if frame.get('id') == rid:
                check('result' in frame, frame)
                return frame['result']

    def retain_native_handles(self, native):
        # Windows acceptance environment: retain process handles while the
        # bridge is alive, so PID reuse/exit cannot invalidate the cleanup test.
        if os.name != 'nt':
            return
        command = "Get-CimInstance Win32_Process | Where-Object {$_.ParentProcessId -eq " + str(self.p.pid) + "} | Select-Object ProcessId,ExecutablePath | ConvertTo-Json -Compress"
        result = subprocess.run(['powershell.exe', '-NoProfile', '-Command', command],
            capture_output=True, text=True, timeout=15, check=True)
        children = json.loads(result.stdout or '[]')
        if isinstance(children, dict):
            children = [children]
        matches = [p for p in children if p.get('ExecutablePath') and
                   Path(p['ExecutablePath']).resolve() == native]
        check(matches, ('no actual native child found under bridge', children))
        for child in matches:
            self.handles.append((child['ProcessId'], Bridge.open_pid_handle(child['ProcessId'])))

    def close(self):
        forced = False
        cleanup = {'pid': self.p.pid, 'native_pids': [pid for pid, _ in self.handles],
                   'result': 'failed'}
        started = time.monotonic()
        try:
            if self.p.poll() is None:
                self.p.stdin.close()
                try:
                    self.p.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    forced = True
                    # Kill only this fixture's owned tree, never image-name kills.
                    if os.name == 'nt':
                        subprocess.run(['taskkill', '/PID', str(self.p.pid), '/T', '/F'],
                                       capture_output=True, timeout=10)
                    else:
                        self.p.kill()
                    self.p.wait(timeout=10)
            errors = []
            for pid, handle in self.handles:
                try:
                    Bridge.wait_handle_exit(handle, timeout=10)
                except Exception as error:
                    errors.append(f'{pid}: {error}')
                    if os.name == 'nt':
                        subprocess.run(['taskkill', '/PID', str(pid), '/T', '/F'],
                                       capture_output=True, timeout=10)
            self.handles.clear()
            check(not errors, ('native process leaked after EOF', errors))
            check(not forced, 'owned process tree required forced cleanup after EOF')
            check(self.p.returncode == 0, ('process failed', self.p.returncode))
            cleanup['result'] = 'passed'
        finally:
            cleanup.update(forced=forced, exit_code=self.p.poll(), seconds=time.monotonic() - started)
            dump(self.root / 'cleanup.json', cleanup)
            self.stderr.close()
            self.reader.join(timeout=2)


class Model:
    def __init__(self, args):
        self.args = args
        self.captures, self.errors = [], []
        self.child_calls = 0
        self.producer = None
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                try:
                    check(self.path == '/v1/chat/completions', self.path)
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    fixture.captures.append(body)
                    delta, finish = fixture.respond(body)
                except Exception:
                    # Return a valid terminal model response, avoiding provider
                    # retries obscuring a missing real plugin producer.
                    fixture.errors.append(traceback.format_exc())
                    delta, finish = {'content': 'FIXTURE_PRECONDITION_FAILED'}, 'stop'
                events = [
                    {'choices': [{'index': 0, 'delta': {'role': 'assistant'}, 'finish_reason': None}]},
                    {'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
                    {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]},
                ]
                data = (''.join('data: ' + json.dumps(e) + '\n\n' for e in events) + 'data: [DONE]\n\n').encode()
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def respond(self, body):
        messages = body['messages']
        users = [m for m in messages if m.get('role') == 'user']
        if users and CHILD_PROMPT in strings(users[-1]):
            self.child_calls += 1
            return {'content': CHILD_TEXT}, 'stop'
        results = [m for m in messages if m.get('role') == 'tool']
        if results:
            check(CHILD_TEXT in strings(results), ('producer did not return real child model response', results))
            return {'content': ROOT_TEXT}, 'stop'
        tools = [t['function'] for t in body.get('tools', [])]
        check(bool(tools), ('isolated delegation plugin not loaded', tools))
        candidates = [t for t in tools if t['name'] == self.args.producer] if self.args.producer else [
            t for t in tools if t['name'] == 'delegate_task' or 'child' in t['name'].lower()]
        check(len(candidates) == 1, ('missing/ambiguous real child start/poll plugin producer; advertised tools',
                                     [t['name'] for t in tools]))
        tool = candidates[0]
        self.producer = tool['name']
        schema = tool.get('parameters', {})
        properties = schema.get('properties', {})
        if self.args.producer_arguments:
            arguments = json.loads(self.args.producer_arguments.replace('{prompt}', CHILD_PROMPT))
        else:
            keys = [key for key in ('prompt', 'text', 'task') if key in properties]
            check(len(keys) == 1, ('specify --producer-arguments for actual producer schema', schema))
            arguments = {keys[0]: CHILD_PROMPT}
            check(set(schema.get('required', [])) <= arguments.keys(), ('required producer arguments', schema))
        return {'tool_calls': [{'index': 0, 'id': 'native-producer-call', 'type': 'function',
            'function': {'name': self.producer, 'arguments': json.dumps(arguments)}}]}, 'tool_calls'

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def environment(args, root, model):
    workspace = root / 'workspace'
    workspace.mkdir()
    agent = root / 'agent'
    extensions = agent / 'extensions'
    extensions.mkdir(parents=True)
    copied = extensions / args.plugin.name
    shutil.copy2(args.plugin, copied)
    dump(agent / 'models.json', {'providers': {'child-local': {
        'api': 'openai-completions', 'baseUrl': f'http://127.0.0.1:{model.server.server_port}/v1',
        'apiKey': 'local-only', 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False,
        'input': ['text'], 'contextWindow': 1000000, 'maxTokens': 1024}]}}})
    dump(agent / 'settings.json', {'defaultProvider': 'child-local', 'defaultModel': 'fixture',
                                  'defaultThinkingLevel': 'off'})
    env = {k: v for k, v in os.environ.items() if not k.startswith(('RPI_', 'PI_'))
           and not k.endswith(('_API_KEY', '_AUTH_TOKEN'))}
    env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(extensions),
        RPI_BIN=str(args.native), RPI_ACP_SESSION_DIR=str(root / 'sessions'),
        RPI_ACP_NO_AUTO_REGISTER_ZED='1', RPI_ACP_NO_PERMISSION_GATE='1', RPI_OFFLINE='1',
        HOME=str(root), USERPROFILE=str(root), TEMP=str(root), TMP=str(root))
    dump(root / 'isolation.json', {'agent': str(agent), 'extensions': str(extensions),
        'plugin_sha256': hashlib.sha256(copied.read_bytes()).hexdigest(),
        'native': str(args.native), 'workspace': str(workspace)})
    return env


def native_run(args, root, enabled, prompt):
    model = Model(args)
    peer = None
    try:
        env = environment(args, root, model)
        if enabled:
            env['RPI_CHILD_EVENTS'] = '1'
        peer = Peer([str(args.native), '--mode', 'rpc', '--no-session'], env, root)
        ready = peer.receive(time.monotonic() + 20)
        check(ready.get('type') == 'ready' and ready.get('protocol') == 'rpi', ready)
        events = ready.get('supportedEvents', [])
        check((ready.get('childEventsVersion') == 1) == enabled, ready)
        for event in ('child_start', 'child_event', 'child_end'):
            check((event in events) == enabled, ready)
        if not prompt:
            return {'result': 'passed', 'ready': ready, 'eof_cleanup': True}
        peer.send({'type': 'prompt', 'id': 1, 'message': 'Delegate to a model-only native child.'})
        deadline = time.monotonic() + 45
        while not any(f.get('type') == 'agent_settled' for f in peer.frames):
            peer.receive(deadline)
        check(not model.errors, model.errors)
        while not any(f.get('type') == 'child_end' for f in peer.frames):
            peer.receive(deadline)
        check(model.child_calls == 1, ('real child model calls', model.child_calls))
        starts = [f for f in peer.frames if f.get('type') == 'child_start']
        ends = [f for f in peer.frames if f.get('type') == 'child_end']
        check(len(starts) == len(ends) == 1, peer.frames)
        sid = starts[0]['sessionId']
        nested = [f['event'] for f in peer.frames if f.get('type') == 'child_event' and f['sessionId'] == sid]
        commits = [e for e in nested if e.get('type') == 'message_end' and e.get('message', {}).get('role') == 'assistant']
        ids = [e.get('messageId') or e['message'].get('id') for e in commits]
        check(ids and all(isinstance(i, str) and i for i in ids), ('native committed canonical IDs missing', commits))
        inputs = [e for e in nested if e.get('type') == 'message_end' and e.get('message', {}).get('role') == 'user']
        check(len(inputs) == 1 and isinstance(inputs[0].get('messageId'), str) and inputs[0]['messageId'],
              ('native delegated input identity missing', inputs))
        check(CHILD_PROMPT in strings(inputs[0]['message']['content']), inputs)
        check(inputs[0]['messageId'] not in ids, ('input/assistant identity collision', inputs, ids))
        check(CHILD_TEXT in strings(ends[0].get('result')), ends)
        check(peer.frames.index(starts[0]) < next(i for i, f in enumerate(peer.frames) if f.get('type') == 'child_event'), peer.frames)
        check(ends[0]['sessionId'] == sid and ends[0]['parentSessionId'] == starts[0]['parentSessionId'], ends)
        return {'result': 'passed', 'model_calls': len(model.captures), 'native_commit_ids': ids,
                'child_session_id': sid, 'eof_cleanup': True}
    finally:
        try:
            if peer:
                peer.close()
        finally:
            model.close()
            dump(root / 'native-frames.json', peer.frames if peer else [])
            dump(root / 'model-inputs.json', model.captures)
            dump(root / 'model-errors.json', model.errors)


def acp_run(args, root, version, enabled):
    model = Model(args)
    peer = None
    try:
        env = environment(args, root, model)
        # Poison inherited opt-in: baseline v1 must clear even exact "1".
        env['RPI_CHILD_EVENTS'] = '1'
        peer = Peer([str(args.bridge)], env, root)
        caps = {'_meta': {OPT_IN: REVISION}}
        if version == 1 and enabled:
            caps['subagents'] = {}
        init = {'protocolVersion': version, 'clientCapabilities': caps} if version == 1 else {
            'protocolVersion': version, 'info': {'name': 'native-child-test', 'version': '1'}, 'capabilities': caps}
        initialized = peer.request('initialize', init)
        check(initialized['protocolVersion'] == version, initialized)
        roots = [peer.request('session/new', {'cwd': str(root / 'workspace'), 'mcpServers': []})['sessionId']
                 for _ in range(2)]
        peer.retain_native_handles(args.native)
        if os.name == 'nt':
            check(len(peer.handles) == 2, ('expected two real native root processes', len(peer.handles)))
        child_ids, canonical_ids = [], []
        for sid in roots:
            offset = len(peer.frames)
            result = peer.request('session/prompt', {'sessionId': sid,
                'prompt': [{'type': 'text', 'text': 'Delegate to a model-only native child.'}]})
            if version == 1:
                check(result.get('stopReason') == 'end_turn', result)
            else:
                deadline = time.monotonic() + 45
                while not any(f.get('params', {}).get('sessionId') == sid and
                    f['params'].get('update', {}).get('sessionUpdate') == 'state_update' and
                    f['params']['update'].get('state') == 'idle' for f in peer.frames[offset:]):
                    peer.receive(deadline)
            check(not model.errors, model.errors)
            if enabled:
                deadline = time.monotonic() + 15
                # Child forwarding and root settlement are independent tasks.
                # Await the real terminal, never infer it from parent completion.
                while not any(f.get('params', {}).get('sessionId') == sid and
                    f['params'].get('update', {}).get('sessionUpdate') == 'subagent_update' and
                    f['params']['update'].get('state', {}).get('state') == 'idle'
                    for f in peer.frames[offset:]):
                    peer.receive(deadline)
            notes = peer.frames[offset:]
            known, child_text, terminals = {sid}, {}, {}
            for frame in notes:
                if frame.get('method') != 'session/update':
                    continue
                params = frame['params']
                target, update = params['sessionId'], params['update']
                check(target in known, ('child traffic before root announcement or cross-root leak', frame))
                if update['sessionUpdate'] == 'subagent_update':
                    cid = update['sessionId']
                    if cid not in known:
                        check(target == sid and update.get('capabilities') == {}, frame)
                        check(cid.startswith(f'child:{len(sid)}:{sid}:'), frame)
                        known.add(cid)
                        child_ids.append(cid)
                    if update.get('state', {}).get('state') == 'idle':
                        terminals[cid] = terminals.get(cid, 0) + 1
                        check(update['state'].get('stopReason') == 'end_turn', frame)
                if target != sid and update['sessionUpdate'] == 'agent_message_chunk':
                    child_text[target] = child_text.get(target, '') + update['content']['text']
                    if version == 2:
                        mid = update.get('messageId')
                        check(isinstance(mid, str) and mid.strip(), ('missing native canonical ID', frame))
                        canonical_ids.append(mid)
            directed = [f['params'] for f in notes if f.get('method') == 'session/update' and
                        f['params']['update']['sessionUpdate'] in ('session_message', 'session_message_chunk')]
            if enabled:
                check(len(known) == 2 and list(child_text.values()) == [CHILD_TEXT], notes)
                check(list(terminals.values()) == [1], ('expected exactly one child terminal', notes))
                check(len(directed) == 1, ('expected exactly one committed incoming upsert, no chunks/return fabrication', directed))
                incoming = directed[0]
                update = incoming['update']
                check(update['sessionUpdate'] == 'session_message' and incoming['sessionId'] in known - {sid}, incoming)
                check(update.get('senderSessionId') == sid and update.get('recipientSessionId') == incoming['sessionId'], incoming)
                check(isinstance(update.get('messageId'), str) and update['messageId'].strip(), incoming)
                check(isinstance(update.get('content'), list) and CHILD_PROMPT in strings(update['content']), incoming)
                check(all(b.get('type') == 'text' and isinstance(b.get('text'), str) for b in update['content']), incoming)
                check('senderMessageId' not in update and 'recipientMessageId' not in update, incoming)
                assistant_ids = [f['params']['update'].get('messageId') for f in notes
                    if f.get('method') == 'session/update' and f['params']['sessionId'] == incoming['sessionId']
                    and f['params']['update']['sessionUpdate'] in ('agent_message_chunk', 'agent_thought_chunk')]
                check(update['messageId'] not in assistant_ids, ('input/assistant identity collision', incoming, assistant_ids))
            else:
                check(known == {sid} and not child_text and not terminals and not directed, notes)
        check(model.child_calls == 2, ('expected real model-only child per root', model.child_calls))
        if enabled:
            check(len(set(child_ids)) == 2, ('two-root child isolation', child_ids))
        # Leave both real roots open: bridge EOF must reap their native children.
        return {'result': 'passed', 'version': version, 'enabled': enabled,
            'roots': roots, 'children': child_ids, 'model_calls': len(model.captures),
            'child_model_calls': model.child_calls, 'canonical_message_ids': sorted(set(canonical_ids)),
            'eof_cleanup': True, 'native_handle_checks': len(peer.handles)}
    finally:
        try:
            if peer:
                peer.close()
        finally:
            model.close()
            dump(root / 'acp-frames.json', peer.frames if peer else [])
            dump(root / 'model-inputs.json', model.captures)
            dump(root / 'model-errors.json', model.errors)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('bridge', 'native', 'plugin', 'artifacts'):
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--producer', help='Exact real plugin child start/poll tool name; default: unique name containing child')
    parser.add_argument('--producer-arguments', help='JSON tool arguments; {prompt} is replaced by model-only child prompt')
    args = parser.parse_args()
    for name in ('bridge', 'native', 'plugin', 'artifacts'):
        setattr(args, name, getattr(args, name).resolve())
    check(not args.artifacts.exists() or not any(args.artifacts.iterdir()), 'artifacts directory must be empty')
    args.artifacts.mkdir(parents=True, exist_ok=True)
    report = {'result': 'failed', 'artifacts': str(args.artifacts), 'binaries': {}, 'runs': [],
        'limits': ['model-only children; no child tools/UI/permission acceptance',
                   'EOF process cleanup is not child cancellation semantics',
                   'direct native commit IDs and ACP IDs are from separate runs; no synthetic IDs supplied']}
    for path in (args.bridge, args.native, args.plugin):
        report['binaries'][str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
    cases = [('native-default-ready', lambda r: native_run(args, r, False, False)),
             ('native-optin-ready', lambda r: native_run(args, r, True, False)),
             ('native-child', lambda r: native_run(args, r, True, True)),
             ('acp-v1-optin', lambda r: acp_run(args, r, 1, True)),
             ('acp-v2-baseline', lambda r: acp_run(args, r, 2, True)),
             ('acp-v1-default', lambda r: acp_run(args, r, 1, False))]
    for name, run in cases:
        root = args.artifacts / name
        root.mkdir()
        try:
            result = run(root)
        except Exception:
            result = {'result': 'failed', 'error': traceback.format_exc()}
        report['runs'].append({'case': name, **result})
        dump(args.artifacts / 'result.json', report)
        print(name + ': ' + result['result'], flush=True)
    report['result'] = 'passed' if all(r['result'] == 'passed' for r in report['runs']) else 'failed'
    dump(args.artifacts / 'result.json', report)
    print(json.dumps(report, indent=2))
    return int(report['result'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
