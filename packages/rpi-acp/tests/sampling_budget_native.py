#!/usr/bin/env python3
"""Opt-in hostile HTTP-provider budget regressions; never installs a native binary."""
import argparse
import hashlib
import json
from pathlib import Path
import socket
import threading
import time
import uuid

from sampling_native import (ModelServer, Native, KEY, CANARY, OUTPUT, dump,
                             environment, require, sample_request, success)

PIN = '0fcb48b6b46f1ec33dc76956080b9d5734a535190a12fc4fd4c81fd8a360faf5'
BUDGET = 256 * 1024


def run(args):
    root = args.artifacts.resolve()
    require(root.drive.upper() == 'C:', 'artifacts must use an absolute C: path')
    root.mkdir(parents=True, exist_ok=False)
    report = {'binary': str(args.native.resolve()), 'sha256': hashlib.sha256(args.native.read_bytes()).hexdigest(), 'checks': []}
    require(report['sha256'] == args.expected_sha256, 'pinned native hash mismatch')
    server = ModelServer()
    native = None
    attacks = {}

    def route(handler, body):
        marker = next((key for key in attacks if key in json.dumps(body)), None)
        if marker is None:
            return False
        state = attacks[marker]
        authorized = handler.headers.get('Authorization') == 'Bearer ' + KEY
        server.captures.append({'body': body, 'authorizationMatchesConfiguredKey': authorized})
        require(authorized, 'hostile request lost configured authorization')
        require(body.get('max_tokens') == 1, 'fixture requires max_tokens=1')
        handler.send_response(200)
        handler.send_header('Content-Type', 'text/event-stream')
        handler.send_header('Connection', 'close')
        handler.end_headers()
        handler.close_connection = True
        def event(text):
            return ('data: ' + json.dumps({'choices': [{'index': 0, 'delta': {'content': text}, 'finish_reason': None}]}) + '\n\n').encode()
        # Ignore max_tokens deliberately. Control characters expand sixfold in JSON.
        if state['kind'] == 'single':
            chunks = [event('\x00' * (BUDGET // 6 + 1024))]
        else:
            chunks = [event('SAFE_PREFIX')]
            chunks += [event(('\x00' if state['kind'] == 'controls' else 'x') * 2048)] * 200
        state['intendedBytes'] = sum(map(len, chunks))
        require(state['intendedBytes'] > BUDGET, 'hostile body did not exceed budget')
        state['writtenBytes'] = 0
        try:
            for chunk in chunks:
                handler.wfile.write(chunk)
                handler.wfile.flush()
                state['writtenBytes'] += len(chunk)
                time.sleep(.002)
            # Never send a finish frame: rejection must come from the budget,
            # not EOF, malformed JSON, timeout, or a provider finish reason.
            handler.connection.settimeout(.1)
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                try:
                    if handler.connection.recv(1) == b'':
                        state['closed'].set()
                        break
                except socket.timeout:
                    continue
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            state['closed'].set()
        return True

    server.route = route
    def check(name, task):
        try:
            task()
            report['checks'].append({'name': name, 'status': 'pass'})
        except Exception as exc:
            report['checks'].append({'name': name, 'status': 'fail', 'error': str(exc).replace(KEY, '[REDACTED]')})
    try:
        env, workspace = environment(root, server)
        native = Native(args.native.resolve(), root, env, workspace)
        native.receive(lambda f: f.get('type') == 'ready', 20)
        before = success(native.request('get_messages', limit=100))
        errors = []
        for index, kind in enumerate(('chunks', 'single', 'controls', 'chunks')):
            marker = 'HOSTILE_BUDGET_' + str(index)
            state = {'kind': kind, 'closed': threading.Event()}
            attacks[marker] = state
            def attack(marker=marker, state=state):
                request = sample_request(marker)
                request['maxTokens'] = 1
                start = len(native.frames)
                reply = native.request('sample', sampleId=str(uuid.uuid4()), request=request, timeoutMs=10000)
                require(reply.get('success') is False, 'partial provider output reported as success')
                require(reply.get('error') == 'sampling failed', 'not the fixed sampling failure: ' + json.dumps(reply))
                require(len(json.dumps(reply).encode()) <= 1024, 'error frame exceeds 1 KiB')
                require(all(len(json.dumps(f).encode()) <= 1024 for f in native.frames[start:]), 'partial output leaked into RPC frames')
                require(state['closed'].wait(8), 'provider connection closure not observed')
                require(success(native.request('get_messages', limit=100)) == before, 'hostile sample contaminated root transcript')
                errors.append(reply['error'])
            check('hostile_' + str(index) + '_' + kind + '_slot_recovery', attack)
        check('fixed_error_all_four', lambda: require(len(errors) == 4 and len(set(errors)) == 1, 'budget errors not identical across attacks'))
        def sibling():
            reply = success(native.request('sample', sampleId=str(uuid.uuid4()), request=sample_request(), timeoutMs=10000))
            require(reply['content']['text'] == OUTPUT, 'healthy sibling failed after budget rejection')
            require(server.captures[-1]['authorizationMatchesConfiguredKey'], 'sibling authorization lost')
            require(success(native.request('get_messages', limit=100)) == before, 'sibling mutated root')
        check('healthy_sibling_auth_and_isolation_after_four_failures', sibling)
        def foreground():
            success(native.request('prompt', message='ROOT_AFTER_HOSTILE_BUDGET'))
            native.receive(lambda f: f.get('type') == 'agent_settled', 15)
            messages = success(native.request('get_messages', limit=100))
            encoded = json.dumps(messages)
            require('ROOT_MODEL_OK' in encoded, 'foreground model did not recover')
            require('HOSTILE_BUDGET_' not in encoded and 'SAFE_PREFIX' not in encoded, 'sample leaked to root messages')
            require(server.captures[-1]['authorizationMatchesConfiguredKey'], 'root authorization lost')
        check('root_prompt_and_auth_after_failures', foreground)
        check('fixture_no_errors', lambda: require(not server.failures, str(server.failures)))
    except Exception as exc:
        report['checks'].append({'name': 'setup_or_execution', 'status': 'fail', 'error': str(exc).replace(KEY, '[REDACTED]')})
    finally:
        if native:
            check('bounded_shutdown', native.close)
            dump(root / 'rpc.json', native.frames)
            (root / 'stderr.log').write_text(''.join(native.errors).replace(KEY, '[REDACTED]'), encoding='utf-8')
        server.shutdown()
        server.server_close()
        dump(root / 'model-requests.json', server.captures)
        report['attacks'] = {k: {**{a: b for a, b in v.items() if a != 'closed'}, 'connectionClosed': v['closed'].is_set()} for k, v in attacks.items()}
        report['counts'] = {s: sum(c['status'] == s for c in report['checks']) for s in ('pass', 'fail')}
        dump(root / 'results.json', report)
    print(json.dumps(report, indent=2))
    return int(report['counts']['fail'] != 0)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--expected-sha256', default=PIN)
    parser.add_argument('--artifacts', type=Path, required=True)
    raise SystemExit(run(parser.parse_args()))
