#!/usr/bin/env python3
"""Pinned real-native provider budget acceptance; local HTTP only, no installs/builds.
Reuses sampling_native's bounded RPC and HTTP helpers without changing them.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import threading
import time
import uuid

from sampling_native import ModelServer, Native, dump, environment, require, sample_request, success

PIN = 'b1b371966a3dfe6227e863e8a90655059dea86218d49d8bda4ef9564eb241888'
BUDGET = 256 * 1024
OUTPUT = 'SAFE_PREFIX_PROVIDER_BUDGET'
REPORT = Path('C:/Users/bigfish/.cargo/target/acp-provider-budget-final.md')
PROVIDERS = [('anthropic', 'anthropic-messages', '/v1/messages'),
             ('responses', 'openai-responses', '/v1/responses')]


def wire(kind, text=None, phase='start'):
    """Native mapper fixtures: anthropic/mapper.rs and openai_responses.rs."""
    def event(name, **fields):
        return ('event: ' + name + '\ndata: ' + json.dumps(dict(type=name, **fields)) + '\n\n').encode()
    if kind == 'anthropic':
        if phase == 'start':
            return (event('message_start', message={'id': 'msg_budget', 'type': 'message', 'role': 'assistant', 'content': [], 'usage': {'input_tokens': 1, 'output_tokens': 0}})
                    + event('content_block_start', index=0, content_block={'type': 'text', 'text': ''}))
        if phase == 'delta':
            return event('content_block_delta', index=0, delta={'type': 'text_delta', 'text': text})
        return (event('content_block_stop', index=0)
                + event('message_delta', delta={'stop_reason': 'end_turn'}, usage={'output_tokens': 1})
                + event('message_stop'))
    item = {'type': 'message', 'id': 'msg_budget', 'role': 'assistant', 'status': 'in_progress', 'content': []}
    if phase == 'start':
        return (event('response.created', response={'id': 'resp_budget', 'status': 'in_progress', 'output': []})
                + event('response.output_item.added', output_index=0, item=item)
                + event('response.content_part.added', item_id='msg_budget', output_index=0, content_index=0, part={'type': 'output_text', 'text': '', 'annotations': []}))
    if phase == 'delta':
        return event('response.output_text.delta', item_id='msg_budget', output_index=0, content_index=0, delta=text)
    item.update(status='completed', content=[{'type': 'output_text', 'text': OUTPUT, 'annotations': []}])
    return (event('response.output_text.done', item_id='msg_budget', output_index=0, content_index=0, text=OUTPUT)
            + event('response.output_item.done', output_index=0, item=item)
            + event('response.completed', response={'id': 'resp_budget', 'status': 'completed', 'output': [item], 'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}))


class ProviderNative(Native):
    def __init__(self, binary, root, env, workspace, provider, model):
        self.process = subprocess.Popen([str(binary), '--mode', 'rpc', '--session-id', str(uuid.uuid4()), '--session-dir', str(root / 'sessions'), '--provider', provider, '--model', model], cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8')
        self.frames, self.errors, self.raw_sizes = [], [], []
        self.output, self.pending, self.next_id = queue.Queue(), {}, 0
        threading.Thread(target=self.reader, daemon=True).start()
        threading.Thread(target=lambda: self.errors.extend(self.process.stderr), daemon=True).start()

    def reader(self):
        for line in self.process.stdout:
            self.raw_sizes.append(len(line.encode('utf-8')))
            try:
                frame = json.loads(line)
            except ValueError:
                frame = {'decodeError': True}
            self.frames.append(frame)
            self.output.put(frame)


def write_report(root, report):
    report['counts'] = {s: sum(c['status'] == s for c in report['checks']) for s in ('pass', 'fail', 'blocked')}
    dump(root / 'results.json', report)
    rows = ['# Native provider budget acceptance — FINAL', '',
            '**Scope:** new fixture only; no runtime/native edits or builds. Local fake HTTP, no external AI.', '',
            f"Binary: `{report['binary']}`", f"SHA-256: `{report.get('sha256', 'not verified')}`", '',
            f"Checks: **{report['counts']['pass']} pass / {report['counts']['fail']} fail / {report['counts']['blocked']} blocked**.", '',
            '| Check | Status | Detail |', '|---|---|---|']
    rows += [f"| {c['name']} | {c['status']} | {c.get('error', '')} |" for c in report['checks']]
    rows += ['', '## Provider/API matrix', '', '| Configured provider | API | Model ID | HTTP route | Cases |', '|---|---|---|---|---|']
    for kind, api, route in PROVIDERS:
        rows.append(f'| fixture-{kind} | {api} | budget-{kind}-model | {route} | chunks + single event; healthy first and after each |')
    rows += ['', '## Evidence and interpretation', '',
             f'Full machine results: `{root / "results.json"}`. Per-provider RPC, byte/frame sizes, boolean-only auth captures, transcript hashes and server state are retained under the same directory.',
             'Sampling request maxTokens=1. Anthropic wire max_tokens=1; Responses legitimately clamps max_output_tokens to 16 (native provider API minimum). Fixture deliberately ignores both limits.',
             'Each attack uses the exact valid start/delta prefix proven by a preceding healthy sample. Prefix is flushed and held while ping succeeds and no sample response arrives; only then is oversized data released. Attack never sends stop, EOF, or an intentional parse error. Fixed failure alone is not counted as budget evidence: successful writes must exceed 256 KiB, no early rejection is allowed, closure must be observed, and subsequent same-provider/model sampling must succeed.',
             'Native isolated sampling does not expose internal provider deltas; acceptance is inferred from the healthy exact-output control and absence of rejection during the gated valid prefix, not claimed as direct internal parser instrumentation.',
             'Both root get_messages and JSONL transcript SHA-256 snapshots must remain unchanged (native-owned .lock files are excluded). RPC frame sizes include newline. Auth capture stores only presence/match booleans; generated fixture-only keys remain solely in required local config. Child TEMP/TMP/TMPDIR use unique directories under C:/Users/bigfish/.cargo/target/acp-tmp.',
             '', '## Outstanding matrix / parent status', '',
             'Existing OpenAI Completions hostile suite: prior reported 9-pass on this same b1b binary, not rerun or added to these counts. Shared decoder unit coverage is prior evidence, not executed here. Other providers and external AI endpoints are not tested. This exercises real native RPC/provider wrappers, not a fresh ACP transport test.',
             '**Parent overall goal remains INCOMPLETE:** independent GET reverse callback/reconnect/replay, durable phase 2, and browser third-party acceptance remain outstanding. No deployment or overall completion claim.', '']
    REPORT.write_text('\n'.join(rows), encoding='utf-8')


def run(args):
    root = args.artifacts.resolve()
    require(root.drive.upper() == 'C:', 'absolute C: artifacts required')
    root.mkdir(parents=True, exist_ok=False)
    report = {'binary': str(args.native.resolve()), 'expectedSha256': args.expected_sha256, 'checks': [], 'providers': {}}
    keys = []
    def check(name, task):
        try:
            task()
            report['checks'].append({'name': name, 'status': 'pass'})
            return True
        except Exception as exc:
            error = str(exc)
            for key in keys:
                error = error.replace(key, '[REDACTED]')
            report['checks'].append({'name': name, 'status': 'fail', 'error': error})
            return False
    try:
        report['sha256'] = hashlib.sha256(args.native.read_bytes()).hexdigest()
        require(report['sha256'] == args.expected_sha256, 'native SHA-256 mismatch')
        for kind, api, path in PROVIDERS:
            server, native = ModelServer(), None
            states = {}
            key = 'fixture-only-' + uuid.uuid4().hex
            keys.append(key)
            provider, model = 'fixture-' + kind, 'budget-' + kind + '-model'
            dest = root / kind
            detail = {'api': api, 'provider': provider, 'model': model, 'attacks': {}}
            report['providers'][kind] = detail
            def route(handler, body):
                authorized = (handler.headers.get('x-api-key') == key if kind == 'anthropic' else handler.headers.get('Authorization') == 'Bearer ' + key)
                marker = next((m for m in states if m in json.dumps(body)), None)
                server.captures.append({'path': handler.path, 'model': body.get('model'), 'authPresent': bool(handler.headers.get('x-api-key') or handler.headers.get('Authorization')), 'authMatches': authorized, 'max_tokens': body.get('max_tokens'), 'max_output_tokens': body.get('max_output_tokens'), 'attack': marker})
                require(handler.path == path and body.get('model') == model and authorized, 'wrong provider route/model/auth')
                require(body.get('max_tokens' if kind == 'anthropic' else 'max_output_tokens') == (1 if kind == 'anthropic' else 16), 'wrong provider token limit')
                handler.send_response(200)
                handler.send_header('Content-Type', 'text/event-stream')
                handler.send_header('Connection', 'close')
                handler.end_headers()
                handler.close_connection = True
                prefix = wire(kind) + wire(kind, OUTPUT, 'delta')
                if marker is None:
                    handler.wfile.write(prefix + wire(kind, phase='stop'))
                    handler.wfile.flush()
                    return True
                state = states[marker]
                state['writtenBytes'] = 0
                try:
                    handler.wfile.write(prefix)
                    handler.wfile.flush()
                    state['writtenBytes'] += len(prefix)
                    state['prefix'].set()
                    require(state['release'].wait(5), 'prefix gate not released')
                    chunks = ([wire(kind, 'x' * (BUDGET + 4096), 'delta')] if state['kind'] == 'single' else [wire(kind, 'x' * 4096, 'delta')] * 80)
                    state['intendedBytes'] = len(prefix) + sum(map(len, chunks))
                    for chunk in chunks:
                        handler.wfile.write(chunk)
                        handler.wfile.flush()
                        state['writtenBytes'] += len(chunk)
                        time.sleep(.003)
                    handler.connection.settimeout(.1)
                    end = time.monotonic() + 5
                    while time.monotonic() < end:
                        try:
                            if handler.connection.recv(1) == b'':
                                state['closed'].set()
                                break
                        except socket.timeout:
                            pass
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    state['closed'].set()
                return True
            server.route = route
            try:
                env, workspace = environment(dest, server)
                dump(dest / 'agent/models.json', {'providers': {provider: {'api': api, 'baseUrl': server.origin + ('' if kind == 'anthropic' else '/v1'), 'apiKey': key, 'models': [{'id': model, 'name': model, 'reasoning': False, 'input': ['text'], 'contextWindow': 100000, 'maxTokens': 1024}]}}})
                dump(dest / 'agent/settings.json', {'defaultProvider': provider, 'defaultModel': model, 'defaultThinkingLevel': 'off'})
                for name in list(env):
                    if name.endswith('_API_KEY') or name.endswith('_AUTH_TOKEN'):
                        del env[name]
                temp = Path('C:/Users/bigfish/.cargo/target/acp-tmp') / ('provider-budget-' + uuid.uuid4().hex)
                temp.mkdir(parents=True, exist_ok=False)
                env.update(TEMP=str(temp), TMP=str(temp), TMPDIR=str(temp), PYTHONDONTWRITEBYTECODE='1')
                native = ProviderNative(args.native.resolve(), dest, env, workspace, provider, model)
                native.receive(lambda f: f.get('type') == 'ready', 20)
                before = success(native.request('get_messages', limit=100))
                def transcript():
                    return {str(p.relative_to(dest)): hashlib.sha256(p.read_bytes()).hexdigest() for p in (dest / 'sessions').rglob('*.jsonl') if p.is_file()}
                baseline = transcript()
                detail['transcriptBefore'] = baseline
                def isolated():
                    require(success(native.request('get_messages', limit=100)) == before, 'root messages mutated')
                    require(transcript() == baseline, 'root session files mutated')
                def healthy():
                    req = sample_request('HEALTHY_PROVIDER_BUDGET')
                    req['maxTokens'] = 1
                    data = success(native.request('sample', sampleId=str(uuid.uuid4()), request=req, timeoutMs=8000))
                    require(data['content'] == {'type': 'text', 'text': OUTPUT} and data['model'] == model and data['stopReason'] == 'endTurn', 'healthy wire not exact: ' + json.dumps(data))
                    require(server.captures[-1]['authMatches'], 'healthy auth lost')
                    isolated()
                for attack_kind in ('chunks', 'single'):
                    ready = check(kind + '_' + attack_kind + '_healthy_first', healthy)
                    if not ready:
                        report['checks'].append({'name': kind + '_' + attack_kind + '_budget', 'status': 'blocked', 'error': 'healthy wire control failed; no budget claim'})
                        continue
                    marker = 'ATTACK_' + kind + '_' + attack_kind
                    state = {'kind': attack_kind, 'prefix': threading.Event(), 'release': threading.Event(), 'closed': threading.Event()}
                    states[marker] = state
                    def attack():
                        req = sample_request(marker)
                        req['maxTokens'] = 1
                        start = len(native.raw_sizes)
                        ident = native.send('sample', sampleId=str(uuid.uuid4()), request=req, timeoutMs=10000)
                        try:
                            require(state['prefix'].wait(5), 'valid prefix never sent')
                            time.sleep(.3)
                            success(native.request('ping'))
                            require(not any(f.get('id') == ident and f.get('type') == 'response' for f in native.frames), 'protocol rejection before oversize release')
                            state['prefixHeldWithoutRejection'] = True
                        finally:
                            state['release'].set()
                        reply = native.response(ident)
                        state['reply'] = reply
                        state['maxRpcFrameBytes'] = max(native.raw_sizes[start:], default=0)
                        require(reply.get('success') is False and reply.get('error') == 'sampling failed', 'not fixed budget failure: ' + json.dumps(reply))
                        require(state['maxRpcFrameBytes'] <= 1024, 'RPC frame exceeds 1 KiB or partial output leaked')
                        require(state['closed'].wait(6), 'provider connection not closed')
                        require(state['writtenBytes'] > BUDGET, 'failure before written bytes exceeded budget')
                        isolated()
                    check(kind + '_' + attack_kind + '_budget', attack)
                    check(kind + '_' + attack_kind + '_healthy_recovery', healthy)
                check(kind + '_http_fixture_clean', lambda: require(not server.failures, str(server.failures)))
                detail['transcriptAfter'] = transcript()
            except Exception as exc:
                check(kind + '_setup_execution', lambda: require(False, str(exc)))
            finally:
                if native:
                    check(kind + '_bounded_shutdown', native.close)
                    dump(dest / 'rpc.json', native.frames)
                    dump(dest / 'rpc-frame-bytes.json', native.raw_sizes)
                    (dest / 'stderr.log').write_text(''.join(native.errors).replace(key, '[REDACTED]'), encoding='utf-8')
                server.shutdown()
                server.server_close()
                dump(dest / 'http-captures.json', server.captures)
                detail['attacks'] = {m: {**{k: v for k, v in s.items() if k not in ('prefix', 'release', 'closed')}, 'connectionClosed': s['closed'].is_set()} for m, s in states.items()}
                write_report(root, report)
    except Exception as exc:
        check('setup_execution', lambda: require(False, str(exc)))
    finally:
        write_report(root, report)
    print(json.dumps(report, indent=2))
    return int(report['counts']['fail'] + report['counts']['blocked'] != 0)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--expected-sha256', default=PIN)
    parser.add_argument('--artifacts', type=Path, required=True)
    raise SystemExit(run(parser.parse_args()))
