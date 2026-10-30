#!/usr/bin/env python3
"""Genuine native ACP -> HTTP RS/PRM/AS/DCR -> browser redirect -> token -> tool.
Standard library only. Tokens/codes are memory-only; artifacts contain assertions,
not wire URLs or HTTP credentials. Requires explicit native and bridge binaries.
"""
import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import queue
import threading
import time
import urllib.parse as U
import urllib.request as H
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from protocol_regression import Bridge
from client_tools_native import dump, require, text


def run(args, root, version, mode):
    root.mkdir(parents=True)
    workspace = root / 'workspace'
    workspace.mkdir()
    agent = root / 'agent'
    (agent / 'extensions').mkdir(parents=True)
    auth = agent / 'auth.json'
    auth.write_text('{}\n', encoding='utf-8')
    original_auth = auth.read_bytes()
    events, failures, transactions, tokens = [], [], {}, {}
    counts = {'token': 0, 'browser': 0, 'dcr': 0, 'call': 0, 'model': 0}
    done = threading.Event()
    origin = ''

    class Fixture(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, value, status=200, headers=None, content_type='application/json'):
            data = value if isinstance(value, bytes) else json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(data)))
            for key, val in (headers or {}).items():
                self.send_header(key, val)
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            try:
                require(not self.headers.get('Authorization'), 'credential leaked to metadata/browser')
                path = U.urlsplit(self.path)
                if path.path.startswith('/.well-known/oauth-protected-resource'):
                    events.append('prm')
                    self.reply({'resource': origin + '/mcp', 'authorization_servers': [origin + ('/unknown' if mode == 'unknownissuer' else '/issuer')], 'bearer_methods_supported': ['header']})
                elif path.path == '/.well-known/oauth-authorization-server/issuer':
                    events.append('as')
                    self.reply({'issuer': origin + '/issuer', 'authorization_endpoint': origin + '/authorize', 'token_endpoint': origin + '/token', 'registration_endpoint': origin + '/register', 'response_types_supported': ['code'], 'grant_types_supported': ['authorization_code'], 'token_endpoint_auth_methods_supported': ['none'], 'code_challenge_methods_supported': ['S256'], 'scopes_supported': ['read']})
                elif path.path == '/authorize':
                    counts['browser'] += 1
                    q = dict(U.parse_qsl(path.query))
                    require(q['resource'] == origin + '/mcp' and q['scope'] == 'read', 'resource/scope mismatch')
                    require(q['code_challenge_method'] == 'S256', 'PKCE absent')
                    code = os.urandom(24).hex()
                    transactions[code] = q
                    callback = q['redirect_uri'] + '?' + U.urlencode({'code': code, 'state': 'wrong' if mode == 'wrongstate' else q['state'], 'iss': origin + '/issuer'})
                    self.reply({}, 302, {'Location': callback})
                else:
                    raise AssertionError('unexpected GET path')
            except Exception as exc:
                failures.append(str(exc))
                self.reply({}, 500)

        def do_POST(self):
            try:
                body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                if self.path == '/register':
                    counts['dcr'] += 1
                    v = json.loads(body)
                    require(v['token_endpoint_auth_method'] == 'none', 'not public client')
                    self.reply({'client_id': 'fixture-public', 'redirect_uris': v['redirect_uris'], 'token_endpoint_auth_method': 'none', 'grant_types': ['authorization_code'], 'response_types': ['code']}, 201)
                elif self.path == '/token':
                    counts['token'] += 1
                    v = dict(U.parse_qsl(body.decode()))
                    q = transactions[v['code']]
                    require(v['resource'] == origin + '/mcp' and v['redirect_uri'] == q['redirect_uri'], 'exchange binding mismatch')
                    challenge = base64.urlsafe_b64encode(hashlib.sha256(v['code_verifier'].encode()).digest()).decode().rstrip('=')
                    require(challenge == q['code_challenge'], 'PKCE mismatch')
                    if mode == 'tokenerror':
                        self.reply({'error': 'invalid_grant'}, 400)
                    else:
                        token = os.urandom(32).hex()
                        tokens[token] = q['state']
                        self.reply({'access_token': token, 'token_type': 'Bearer', 'expires_in': 300, 'scope': 'read'})
                elif self.path == '/mcp':
                    v = json.loads(body)
                    credential = self.headers.get('Authorization', '').removeprefix('Bearer ')
                    if credential not in tokens:
                        events.append('challenge')
                        self.reply({}, 401, {'WWW-Authenticate': 'Bearer scope="read"'})
                        return
                    method = v['method']
                    events.append(method)
                    if 'id' not in v:
                        self.reply(b'', 202)
                        return
                    if method == 'initialize':
                        result = {'protocolVersion': v['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'oauth-native', 'version': '1'}}
                    elif method == 'tools/list':
                        result = {'tools': [{'name': 'echo', 'description': 'OAuth native proof tool', 'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}}]}
                    elif method == 'tools/call':
                        counts['call'] += 1
                        require(v['params']['name'] == 'echo', 'wrong discovered tool')
                        result = {'content': [{'type': 'text', 'text': 'OAUTH_NATIVE_TOOL_RESULT'}]}
                    else:
                        raise AssertionError('unexpected MCP method')
                    self.reply({'jsonrpc': '2.0', 'id': v['id'], 'result': result})
                elif self.path == '/v1/chat/completions':
                    counts['model'] += 1
                    v = json.loads(body)
                    tools = [t['function'] for t in v.get('tools', []) if 'OAuth native proof' in t['function'].get('description', '')]
                    require(len(tools) == 1, 'native model missing discovered schema')
                    results = [m for m in v['messages'] if m.get('role') == 'tool' and m.get('tool_call_id') == 'oauth-proof']
                    if results:
                        require('OAUTH_NATIVE_TOOL_RESULT' in text(results), 'native model missing real tool result')
                        delta, finish = {'content': 'OAUTH_NATIVE_DONE'}, 'stop'
                        done.set()
                    else:
                        delta = {'tool_calls': [{'index': 0, 'id': 'oauth-proof', 'type': 'function', 'function': {'name': tools[0]['name'], 'arguments': '{}'}}]}
                        finish = 'tool_calls'
                    chunks = [{'choices': [{'index': 0, 'delta': {'role': 'assistant'}, 'finish_reason': None}]}, {'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]}, {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]}]
                    self.reply((''.join('data: ' + json.dumps(c) + '\n\n' for c in chunks) + 'data: [DONE]\n\n').encode(), content_type='text/event-stream')
                else:
                    raise AssertionError('unexpected POST path')
            except Exception as exc:
                failures.append(str(exc))
                self.reply({}, 500)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
    origin = f'http://127.0.0.1:{server.server_port}'
    threading.Thread(target=server.serve_forever, daemon=True).start()
    policy = root / 'policy.json'
    dump(policy, {'resources': [{'resource': origin + '/mcp', 'issuer': origin + '/issuer', 'scopes': ['read'], 'allowDynamicRegistration': True}], 'network': {'origins': [], 'developmentLoopbackOrigins': [origin]}})
    if mode == 'configdenial':
        value = json.loads(policy.read_text(encoding='utf-8'))
        value['resources'][0]['allowDynamicRegistration'] = False
        dump(policy, value)
    dump(agent / 'models.json', {'providers': {'local': {'api': 'openai-completions', 'baseUrl': origin + '/v1', 'apiKey': 'local-only', 'models': [{'id': 'fixture', 'name': 'fixture', 'reasoning': False, 'input': ['text'], 'contextWindow': 100000, 'maxTokens': 1024}]}}})
    dump(agent / 'settings.json', {'defaultProvider': 'local', 'defaultModel': 'fixture', 'defaultThinkingLevel': 'off'})
    env = dict(RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / 'extensions'), RPI_OFFLINE='1', HOME=str(root), USERPROFILE=str(root), RPI_ACP_MCP_OAUTH_POLICY=str(policy))
    bridge = Bridge(args.bridge, args.native, root / 'sessions', workspace, extra_env=env)
    frames, roots = [], []
    try:
        caps = {'elicitation': {'url': {}}}
        init = {'protocolVersion': version, **({'clientCapabilities': caps} if version == 1 else {'info': {'name': 'oauth-native', 'version': '1'}, 'capabilities': caps})}
        reply, _ = bridge.request('initialize', init)
        require(reply.get('result', {}).get('protocolVersion') == version, reply)
        for index in range(2 if mode == 'success' else 1):
            request_id = 500 + index
            bridge.send({'jsonrpc': '2.0', 'id': request_id, 'method': 'session/new', 'params': {'cwd': str(workspace), 'mcpServers': [{'type': 'http', 'name': 'oauth', 'url': origin + '/mcp', 'headers': []}]}})
            elicitation = None
            deadline = time.monotonic() + 35
            while True:
                frame = bridge.output.get(timeout=max(.01, deadline - time.monotonic()))
                frames.append(frame)
                if frame.get('method') == 'elicitation/create':
                    require(elicitation is None, 'duplicate OAuth prompt')
                    elicitation = frame
                    sid = frame['params']['sessionId']
                    require(sid not in roots and frame['params']['mode'] == 'url', 'cross-root OAuth cache/UI reuse')
                    if mode in ['cancel', 'eof']:
                        if mode == 'eof':
                            bridge.process.stdin.close()
                        else:
                            bridge.send({'jsonrpc': '2.0', 'method': '$/cancel_request', 'params': {'requestId': request_id}})
                        continue
                    bridge.send({'jsonrpc': '2.0', 'id': frame['id'], 'result': {'action': 'decline' if mode == 'decline' else 'accept'}})
                    if mode != 'decline':
                        # Acceptance is only an acknowledgement; no token before browser GET.
                        time.sleep(.1)
                        require(counts['token'] == index, 'UI acknowledgement granted credentials')
                        try:
                            with H.urlopen(frame['params']['url'], timeout=8) as response:
                                response.read()
                        except (H.HTTPError, http.client.RemoteDisconnected):
                            require(mode == 'wrongstate', 'browser callback rejected')
                if mode == 'eof' and frame.get('method') == '$/cancel_request':
                    require(frame['params']['requestId'] == elicitation['id'], 'wrong canonical UI cancellation id')
                    bridge.process.wait(timeout=15)
                    break
                if frame.get('id') == request_id and 'method' not in frame:
                    if mode == 'success':
                        require('result' in frame and elicitation is not None, frame)
                        sid = frame['result']['sessionId']
                        require(sid == elicitation['params']['sessionId'], 'provisional root differs from final native root')
                        roots.append(sid)
                    else:
                        require('error' in frame, 'failed authorization opened session')
                    break
            if mode == 'success':
                done.clear()
                reply, updates = bridge.request('session/prompt', {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'Use the OAuth native proof tool then report result.'}]})
                frames.extend(updates)
                require('result' in reply, reply)
                require(done.wait(15), 'real native model did not consume tool result')
                reply, _ = bridge.request('session/close', {'sessionId': sid})
                require('result' in reply, reply)
            elif mode == 'cancel':
                # Drain cancellation notification; late response cannot restart OAuth.
                if not any(f.get('method') == '$/cancel_request' for f in frames):
                    frames.append(bridge.output.get(timeout=5))
                require(any(f.get('method') == '$/cancel_request' and f['params']['requestId'] == elicitation['id'] for f in frames), 'missing owned UI cancellation')
                bridge.send({'jsonrpc': '2.0', 'id': elicitation['id'], 'result': {'action': 'accept'}})
                reply, updates = bridge.request('session/list', {})
                require('result' in reply, reply)
                require(not any(f.get('method') == 'elicitation/create' for f in updates), 'late answer reprompted')
        require(not failures, failures)
        require(auth.read_bytes() == original_auth, 'native auth.json mutated')
        if mode == 'success':
            require(counts['dcr'] == 2 and counts['browser'] == 2 and counts['token'] == 2 and counts['call'] == 2, counts)
            require('prm' in events and 'as' in events and 'tools/list' in events, 'missing genuine discovery')
        elif mode in ['cancel', 'eof', 'decline', 'wrongstate', 'unknownissuer', 'configdenial']:
            require(counts['token'] == 0 and counts['call'] == 0, 'cancel/denial committed token')
        elif mode == 'tokenerror':
            require(counts['token'] == 1 and counts['call'] == 0, counts)
        if mode in ['unknownissuer', 'configdenial']:
            require(counts['dcr'] == 0 and counts['browser'] == 0, 'unknown issuer caused trusted side effect')
    finally:
        if bridge.process.stdin.closed:
            bridge.process.wait(timeout=15)
        else:
            bridge.close()
        server.shutdown()
        server.server_close()
    secrets = list(tokens) + list(transactions)
    for path in (root / 'sessions').rglob('*.jsonl'):
        content = path.read_text(encoding='utf-8')
        require(not any(secret in content for secret in secrets), 'OAuth secret persisted')
    dump(root / 'result.json', {'version': version, 'mode': mode, 'counts': counts, 'events': events, 'pass': True})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--modes', nargs='+', default=['success', 'cancel', 'decline', 'wrongstate', 'tokenerror', 'unknownissuer', 'configdenial', 'eof'])
    args = parser.parse_args()
    args.bridge, args.native, args.artifacts = args.bridge.resolve(), args.native.resolve(), args.artifacts.resolve()
    for version in [1, 2]:
        for mode in args.modes:
            run(args, args.artifacts / f'v{version}-{mode}', version, mode)
            print(f'PASS v{version} {mode}', flush=True)


if __name__ == '__main__':
    main()
