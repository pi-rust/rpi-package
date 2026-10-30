#!/usr/bin/env python3
"""Real native JSONL sampling acceptance. Standard library; never installs or deletes.
Artifacts and all child temporary files stay under --artifacts on C:. Assertions
are explicit (not disabled by -O). The configured model key is never serialized.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import threading
import time
import traceback
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PIN = '4acbea81ed84c57f738bd379431fc3c3f2e5b42b5970f52ad0ae9c92089201eb'
REMOTE = 'REMOTE_SAMPLE_SYSTEM_721bb'
PROMPT = 'REMOTE_SAMPLE_USER_721bb'
OUTPUT = 'ACTUAL_MODEL_SAMPLE_721bb'
CANARY = 'FOREGROUND_SECRET_CANARY_721bb'
KEY = 'local-only'


def require(value, message):
    if not value:
        raise AssertionError(message)


def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')


def wait_until(predicate, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.02)
    raise AssertionError('bounded wait expired')


class ModelServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self):
        super().__init__(('127.0.0.1', 0), ModelHandler)
        self.captures = []
        self.failures = []
        self.stalls = {}
        self.route = None
        threading.Thread(target=self.serve_forever, daemon=True).start()

    @property
    def origin(self):
        return 'http://127.0.0.1:' + str(self.server_port)


class ModelHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def sse(self, delta, finish='stop'):
        chunks = [{'choices': [{'index': 0, 'delta': {'role': 'assistant'}, 'finish_reason': None}]},
                  {'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
                  {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]}]
        data = (''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if self.server.route and self.server.route(self, body):
                return
            require(self.path == '/v1/chat/completions', 'unexpected model path')
            authorized = self.headers.get('Authorization') == 'Bearer ' + KEY
            self.server.captures.append({'body': body, 'authorizationMatchesConfiguredKey': authorized})
            require(authorized, 'configured model Authorization missing/mismatched')
            encoded = json.dumps(body)
            marker = next((s for s in self.server.stalls if s in encoded), None)
            if marker:
                state = self.server.stalls[marker]
                state['received'].set()
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                self.wfile.write(b': connected\n\n')
                self.wfile.flush()
                self.connection.settimeout(.1)
                deadline = time.monotonic() + 35
                while time.monotonic() < deadline:
                    if state.get('release') and state['release'].is_set():
                        chunks = [{'choices': [{'index': 0, 'delta': {'role': 'assistant', 'content': OUTPUT}, 'finish_reason': None}]}, {'choices': [{'index': 0, 'delta': {}, 'finish_reason': 'stop'}]}]
                        self.wfile.write((''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n').encode())
                        self.wfile.flush()
                        self.close_connection = True
                        return
                    try:
                        if self.connection.recv(1) == b'':
                            state['closed'].set()
                            return
                    except socket.timeout:
                        continue
                    except OSError:
                        state['closed'].set()
                        return
                raise AssertionError('stalled model connection did not close')
            self.sse({'content': OUTPUT if REMOTE in encoded else 'ROOT_MODEL_OK'})
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as exc:
            self.server.failures.append(str(exc))
            self.send_error(500)


def environment(root, server):
    root.mkdir(parents=True, exist_ok=True)
    agent = root / 'agent'
    (agent / 'extensions').mkdir(parents=True)
    workspace = root / 'workspace'
    workspace.mkdir()
    (workspace / 'AGENTS.md').write_text(CANARY, encoding='utf-8')
    (workspace / 'secret.txt').write_text(CANARY, encoding='utf-8')
    dump(agent / 'auth.json', {})
    dump(agent / 'models.json', {'providers': {'local': {'api': 'openai-completions', 'baseUrl': server.origin + '/v1', 'apiKey': KEY, 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False, 'input': ['text'], 'contextWindow': 100000, 'maxTokens': 1024}]}}})
    dump(agent / 'settings.json', {'defaultProvider': 'local', 'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'})
    # The required native config contains the fixture key. It is not an artifact
    # snapshot; logs/captures only retain a boolean, and no real key is used.
    temp = root / 'temp'
    temp.mkdir()
    env = os.environ.copy()
    for key in list(env):
        if key.startswith('RPI_ACP_MCP_'):
            del env[key]
    env.update(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / 'extensions'), RPI_OFFLINE='1', HOME=str(root), USERPROFILE=str(root), TEMP=str(temp), TMP=str(temp), TMPDIR=str(temp))
    return env, workspace


class Native:
    def __init__(self, binary, root, env, workspace):
        self.process = subprocess.Popen([str(binary), '--mode', 'rpc', '--session-id', str(uuid.uuid4()), '--session-dir', str(root / 'sessions'), '--provider', 'local', '--model', 'fixture'], cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8')
        self.frames, self.errors = [], []
        self.output = queue.Queue()
        self.pending = {}
        self.next_id = 0
        threading.Thread(target=self.reader, daemon=True).start()
        threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True).start()

    def reader(self):
        for line in self.process.stdout:
            try:
                frame = json.loads(line)
            except ValueError:
                frame = {'decodeError': True}
            self.frames.append(frame)
            self.output.put(frame)

    def receive(self, predicate, timeout=12):
        end = time.monotonic() + timeout
        deferred = []
        try:
            while True:
                frame = self.output.get(timeout=max(.01, end - time.monotonic()))
                require(not frame.get('decodeError'), 'non-JSON stdout')
                if predicate(frame):
                    return frame
                deferred.append(frame)
        finally:
            for frame in deferred:
                self.output.put(frame)

    def send(self, kind, **kwargs):
        self.next_id += 1
        self.process.stdin.write(json.dumps(dict(id=self.next_id, type=kind, **kwargs)) + '\n')
        self.process.stdin.flush()
        return self.next_id

    def response(self, ident, timeout=12):
        return self.receive(lambda f: f.get('id') == ident and f.get('type') == 'response', timeout)

    def request(self, kind, **kwargs):
        return self.response(self.send(kind, **kwargs))

    def close(self):
        if self.process.poll() is None:
            if not self.process.stdin.closed:
                self.process.stdin.close()
            try:
                self.process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise AssertionError('native failed bounded EOF shutdown; killed test-owned process')


def sample_request(marker=PROMPT):
    return {'messages': [{'role': 'user', 'content': {'type': 'text', 'text': marker}}], 'systemPrompt': REMOTE, 'maxTokens': 37, 'temperature': .25, 'includeContext': 'none'}


def success(frame):
    require(frame.get('success') is True, 'RPC failed: ' + json.dumps(frame))
    return frame.get('data')


def run(args):
    root = args.artifacts.resolve()
    require(root.drive.upper() == 'C:', 'artifacts must be on C:')
    require(root.name.startswith('acp-sampling-native-run-'), 'use acp-sampling-native-run-* artifact directory')
    root.mkdir(parents=True, exist_ok=False)
    report = {'binary': str(args.native.resolve()), 'expectedSha256': args.expected_sha256, 'checks': []}
    server = native = None

    def check(name, task):
        try:
            task()
            report['checks'].append({'name': name, 'status': 'pass'})
        except Exception as exc:
            report['checks'].append({'name': name, 'status': 'fail', 'error': str(exc).replace(KEY, '[REDACTED]')})

    try:
        require(args.native.is_file(), 'pinned native binary missing')
        report['sha256'] = hashlib.sha256(args.native.read_bytes()).hexdigest()
        require(report['sha256'] == args.expected_sha256, 'native SHA256 mismatch')
        server = ModelServer()
        env, workspace = environment(root, server)
        native = Native(args.native.resolve(), root, env, workspace)
        native.receive(lambda f: f.get('type') == 'ready', 20)
        before = success(native.request('get_messages', limit=100))
        state = success(native.request('get_state'))

        def isolation():
            result = success(native.request('sample', sampleId=str(uuid.uuid4()), request=sample_request(), timeoutMs=10000))
            require(result == {'role': 'assistant', 'model': 'fixture', 'content': {'type': 'text', 'text': OUTPUT}, 'stopReason': 'endTurn'}, 'sample result not exact actual model output: ' + json.dumps(result))
            capture = server.captures[-1]
            require(capture['authorizationMatchesConfiguredKey'], 'credentials lost in isolation')
            body = capture['body']
            require(body['max_tokens'] == 37 and body['temperature'] == .25, 'sampling limits changed')
            require('tools' not in body, 'isolated request contains tool schemas')
            require(body['messages'] == [{'role': 'system', 'content': REMOTE}, {'role': 'user', 'content': PROMPT}], 'isolated model messages not exactly remote system/user')
            require(CANARY not in json.dumps(body), 'foreground resources leaked')
        check('actual_model_authorization_exact_limits_and_isolation', isolation)
        check('root_messages_unchanged', lambda: require(success(native.request('get_messages', limit=100)) == before, 'root history mutated'))
        check('root_configuration_unchanged', lambda: require(success(native.request('get_state')) == state, 'root state/config mutated'))

        def validation():
            count = len(server.captures)
            for patch in ({'tools': []}, {'toolChoice': 'auto'}, {'includeContext': 'allServers'}, {'stopSequences': ['x']}, {'maxTokens': 0}, {'maxTokens': 16385}):
                request = sample_request()
                request.update(patch)
                reply = native.request('sample', sampleId=str(uuid.uuid4()), request=request, timeoutMs=1000)
                require(reply.get('success') is False, 'invalid sampling accepted: ' + str(patch))
            require(len(server.captures) == count, 'invalid sampling contacted model')
        check('invalid_parameters_zero_model_calls', validation)

        def stalled(marker):
            state = {'received': threading.Event(), 'closed': threading.Event()}
            server.stalls[marker] = state
            return state

        def cancellation():
            marker = 'STALL_CANCEL'
            connection = stalled(marker)
            sid = str(uuid.uuid4())
            ident = native.send('sample', sampleId=sid, request=sample_request(marker), timeoutMs=15000)
            require(connection['received'].wait(8), 'stalled request never reached server')
            sibling = native.send('sample', sampleId=str(uuid.uuid4()), request=sample_request(), timeoutMs=10000)
            success(native.request('cancel_sample', sampleId=sid))
            # Observe closure immediately at acknowledgement, allowing only socket-reader scheduling.
            require(connection['closed'].wait(.3), 'cancel ack preceded model stream cleanup')
            require(native.response(ident).get('success') is False, 'cancelled sample succeeded')
            require(success(native.response(sibling))['content']['text'] == OUTPUT, 'sibling sample cancelled')
            require(native.request('sample', sampleId=sid, request=sample_request()).get('success') is False, 'retired duplicate UUID accepted')
        check('cancel_joins_stream_cleanup_sibling_survives_duplicate_rejected', cancellation)

        def deadline():
            connection = stalled('STALL_DEADLINE')
            reply = native.request('sample', sampleId=str(uuid.uuid4()), request=sample_request('STALL_DEADLINE'), timeoutMs=600)
            require(reply.get('success') is False and 'deadline' in json.dumps(reply), 'deadline not enforced')
            require(connection['received'].is_set() and connection['closed'].wait(.3), 'deadline left model stream alive')
        check('deadline_closes_model_stream', deadline)

        def overflow():
            ids = []
            try:
                for index in range(4):
                    marker = 'STALL_OVERFLOW_' + str(index)
                    connection = stalled(marker)
                    sid = str(uuid.uuid4())
                    ident = native.send('sample', sampleId=sid, request=sample_request(marker), timeoutMs=20000)
                    ids.append((sid, ident))
                    require(connection['received'].wait(5), 'slot did not reach model')
                count = len(server.captures)
                require(native.request('sample', sampleId=str(uuid.uuid4()), request=sample_request()).get('success') is False, 'fifth active sample not rejected')
                require(len(server.captures) == count, 'overflow reached model')
                require(native.request('sample', sampleId=ids[0][0], request=sample_request()).get('success') is False, 'active duplicate accepted')
            finally:
                for sid, ident in ids:
                    success(native.request('cancel_sample', sampleId=sid))
                    native.response(ident)
        check('four_slot_overflow_and_active_duplicate', overflow)

        def root_abort_isolation():
            root_connection = stalled('STALL_FOREGROUND_ABORT')
            root_ident = native.send('prompt', message='STALL_FOREGROUND_ABORT')
            require(root_connection['received'].wait(5), 'foreground root did not reach model')
            connection = stalled('STALL_ROOT_ABORT')
            connection['release'] = threading.Event()
            sid = str(uuid.uuid4())
            ident = native.send('sample', sampleId=sid, request=sample_request('STALL_ROOT_ABORT'), timeoutMs=10000)
            require(connection['received'].wait(5), 'unrelated sample did not reach model')
            success(native.request('abort'))
            require(root_connection['closed'].wait(.3), 'root abort did not close foreground stream')
            native.response(root_ident)
            settled = native.receive(lambda f: f.get('type') == 'agent_settled', 10)
            require(settled.get('outcome') == 'aborted', 'foreground root did not settle aborted')
            server.stalls.pop('STALL_FOREGROUND_ABORT')
            require(not connection['closed'].is_set(), 'root abort closed unrelated sampling stream')
            connection['release'].set()
            require(success(native.response(ident))['content']['text'] == OUTPUT, 'root abort cancelled unrelated isolated sample')
        check('root_abort_does_not_cancel_unrelated_sample', root_abort_isolation)

        def root_prompt():
            success(native.request('prompt', message='Reply ROOT_MODEL_OK. ' + CANARY))
            settled = native.receive(lambda f: f.get('type') == 'agent_settled', 15)
            require(settled.get('outcome') == 'completed', 'root prompt did not settle completed')
            require(any('ROOT_MODEL_OK' in json.dumps(f) for f in native.frames), 'root model output absent')
        check('real_root_prompt_after_sampling', root_prompt)

        def transcripts():
            files = list((root / 'sessions').rglob('*.jsonl'))
            require(files, 'native transcript missing')
            for path in files:
                data = path.read_text(encoding='utf-8')
                require(all(value not in data for value in (REMOTE, PROMPT, OUTPUT)), 'isolated sampling persisted in root transcript')
        check('native_transcript_excludes_sampling', transcripts)

        def eof():
            connection = stalled('STALL_EOF')
            native.send('sample', sampleId=str(uuid.uuid4()), request=sample_request('STALL_EOF'), timeoutMs=20000)
            require(connection['received'].wait(5), 'EOF sample did not start')
            native.close()
            require(connection['closed'].wait(.3), 'EOF left stream alive')
        check('EOF_reaps_native_and_model_stream', eof)
        check('fixture_has_no_internal_failures', lambda: require(not server.failures, str(server.failures)))
    except Exception as exc:
        report['checks'].append({'name': 'prerequisites_or_setup', 'status': 'blocked', 'error': str(exc).replace(KEY, '[REDACTED]')})
    finally:
        if native:
            check('bounded_native_cleanup', native.close)
            dump(root / 'rpc.json', native.frames)
            (root / 'stderr.log').write_text(''.join(native.errors).replace(KEY, '[REDACTED]'), encoding='utf-8')
        if server:
            dump(root / 'model-requests.json', server.captures)
            server.shutdown()
            server.server_close()
        report['counts'] = {status: sum(c['status'] == status for c in report['checks']) for status in ('pass', 'fail', 'blocked')}
        dump(root / 'results.json', report)
    print(json.dumps(report, indent=2))
    return 0 if report['counts']['fail'] == report['counts']['blocked'] == 0 else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--expected-sha256', default=PIN, help='Explicit trusted native SHA256 (default: original pinned binary)')
    parser.add_argument('--bridge', type=Path)
    parser.add_argument('--artifacts', type=Path, required=True)
    return run(parser.parse_args())


if __name__ == '__main__':
    raise SystemExit(main())
