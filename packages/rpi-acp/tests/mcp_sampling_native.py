#!/usr/bin/env python3
"""Actual ACP -> native -> MCP reverse sampling and catalog acceptance.
Only the HTTP MCP/model peer is simulated. No fake native output, OAuth, pip,
installation, artifact deletion, or runtime edits. Omit --bridge to record the
final parent rebuild as pending. Uses the repository's mcp_protocol.Bridge.
"""
import argparse
import hashlib
import json
from pathlib import Path
import queue
import ctypes
from ctypes import wintypes
import socket
import threading
import time
import uuid
from mcp_protocol import Bridge
from sampling_native import (PIN, KEY, REMOTE, PROMPT, OUTPUT, CANARY, ModelServer,
                             environment, dump, require, sample_request)


def native_handles(parent_pid):
    """Capture direct native children by immutable kernel handle, not PID liveness."""
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    class Entry(ctypes.Structure):
        _fields_ = [('dwSize', wintypes.DWORD), ('cntUsage', wintypes.DWORD),
                    ('th32ProcessID', wintypes.DWORD), ('th32DefaultHeapID', ctypes.c_size_t),
                    ('th32ModuleID', wintypes.DWORD), ('cntThreads', wintypes.DWORD),
                    ('th32ParentProcessID', wintypes.DWORD), ('pcPriClassBase', wintypes.LONG),
                    ('dwFlags', wintypes.DWORD), ('szExeFile', wintypes.WCHAR * 260)]
    kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
    require(snapshot != ctypes.c_void_p(-1).value, 'process snapshot failed')
    found = []
    try:
        entry = Entry()
        entry.dwSize = ctypes.sizeof(entry)
        more = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
        while more:
            if entry.th32ParentProcessID == parent_pid:
                handle = kernel.OpenProcess(0x100000 | 0x1000, False, entry.th32ProcessID)
                require(handle, 'cannot open native child handle')
                found.append((entry.th32ProcessID, handle))
            more = kernel.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return found


def wait_native_handles(handles):
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    records = []
    try:
        for pid, handle in handles:
            waited = kernel.WaitForSingleObject(handle, 10000)
            code = wintypes.DWORD()
            queried = kernel.GetExitCodeProcess(handle, ctypes.byref(code))
            records.append({'pid': pid, 'waitResult': waited, 'exitCode': code.value})
            require(waited == 0 and queried and code.value != 259, 'native handle still active after ACP EOF: ' + str(pid))
    finally:
        for _, handle in handles:
            kernel.CloseHandle(handle)
        handles.clear()
    return records

MARKER = 'REAL_MCP_TOOL_RESULT_2bafe'
RESOURCE = 'fixture://actual/resource'
UNTRUSTED = 'UNTRUSTED_REMOTE_CATALOG_DATA_2bafe'


class Fixture:
    def __init__(self, mode):
        self.mode = mode
        self.server = ModelServer()
        self.server.route = self.route
        self.events = []
        self.replies = []
        self.callbacks = {}
        self.model_bodies = []
        self.initializations = []
        self.lock = threading.Lock()
        self.consent_count = 0
        self.root_results = 0
        self.sampling_calls = 0

    def reply(self, handler, value, status=200):
        data = json.dumps(value).encode() if value is not None else b''
        handler.send_response(status)
        handler.send_header('Content-Type', 'application/json')
        handler.send_header('Content-Length', str(len(data)))
        handler.end_headers()
        handler.wfile.write(data)

    def route(self, handler, body):
        if handler.path == '/v1/chat/completions':
            require(handler.headers.get('Authorization') == 'Bearer ' + KEY, 'native model credentials lost')
            self.model_bodies.append({'body': body, 'authorizationMatchesConfiguredKey': True})
            system = [m for m in body['messages'] if m['role'] in ('system', 'developer')]
            if REMOTE in json.dumps(system):
                self.sampling_calls += 1
                require('tools' not in body, 'sampling inherited native tool schemas')
                require(CANARY not in json.dumps(body), 'sampling inherited foreground canary')
                require(body['messages'] == [{'role': 'system', 'content': REMOTE}, {'role': 'user', 'content': PROMPT}], 'sampling messages not isolated')
                require(body['max_tokens'] == 37 and body['temperature'] == .25, 'sampling parameters changed')
                if self.mode == 'cancel-model':
                    # Fall through to ModelHandler's actual stalled-stream socket probe.
                    return False
                handler.sse({'content': OUTPUT})
                return True
            tools = [t['function'] for t in body.get('tools', [])]
            results = [m for m in body['messages'] if m.get('role') == 'tool']
            if self.mode in ('catalog', 'tools-only'):
                catalog = [t for t in tools if t['name'].startswith('mcp_catalog_')]
                if self.mode == 'tools-only':
                    require(not catalog, 'tools-only server gained catalog descriptors')
                    handler.sse({'content': 'TOOLS_ONLY_OK'})
                    self.root_results += 1
                    return True
                for operation in ('resources_list', 'resources_read', 'prompts_list', 'prompts_get'):
                    require(len([t for t in catalog if operation in t.get('description', '')]) == 1, 'missing/ambiguous actual catalog descriptor ' + operation)
                require(UNTRUSTED not in json.dumps(system) and RESOURCE not in json.dumps(system), 'catalog content promoted to root system instructions')
                operations = [('resources_list', {}), ('resources_read', {'uri': RESOURCE}), ('prompts_list', {}), ('prompts_get', {'name': 'actual-prompt'})]
                if len(results) < len(operations):
                    operation, arguments = operations[len(results)]
                    selected = next(t for t in catalog if operation in t.get('description', ''))
                    handler.sse({'tool_calls': [{'index': 0, 'id': 'catalog-' + str(len(results)), 'type': 'function', 'function': {'name': selected['name'], 'arguments': json.dumps(arguments)}}]}, 'tool_calls')
                else:
                    encoded = json.dumps(results)
                    require(RESOURCE in encoded and UNTRUSTED in encoded and 'assistant' in encoded and 'user' in encoded, 'actual resource URI / prompt role-content missing from native model tool results')
                    handler.sse({'content': 'CATALOG_NATIVE_OK'})
                    self.root_results += 1
                return True
            if results:
                encoded = json.dumps(results)
                if self.mode in ('success', 'preapproved', 'two-roots') or (self.mode == 'budget' and self.root_results < 2):
                    require(MARKER in encoded, 'actual MCP tool result did not reach native parent model')
                if self.mode == 'budget' and self.root_results == 2:
                    require('SAMPLING_DENIED' in encoded, 'third root did not receive denied tool result')
                handler.sse({'content': 'ACP_NATIVE_FINAL'})
                self.root_results += 1
            else:
                matches = [t for t in tools if 'Native reverse sampling acceptance tool' in t.get('description', '')]
                require(len(matches) == 1, 'actual hashed MCP registered tool missing')
                handler.sse({'tool_calls': [{'index': 0, 'id': 'native-mcp-call', 'type': 'function', 'function': {'name': matches[0]['name'], 'arguments': '{}'}}]}, 'tool_calls')
            return True
        require(handler.path == '/mcp', 'unexpected fixture route')
        require(not handler.headers.get('Authorization'), 'model credential leaked to MCP')
        if 'method' not in body:
            with self.lock:
                self.replies.append(body)
                callback = self.callbacks.get(body.get('id'))
            require(callback is not None, 'reverse response ID escaped request scope')
            callback['reply'] = body
            callback['event'].set()
            self.reply(handler, None, 202)
            return True
        method = body['method']
        self.events.append(method)
        if 'id' not in body:
            self.reply(handler, None, 202)
            return True
        if method == 'initialize':
            self.initializations.append(body['params'])
            caps = {'tools': {}}
            if self.mode != 'tools-only':
                caps.update(resources={}, prompts={})
            result = {'protocolVersion': body['params']['protocolVersion'], 'capabilities': caps, 'serverInfo': {'name': 'native-sampling-catalog', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'sample / remote tool', 'description': 'Native reverse sampling acceptance tool', 'inputSchema': {'type': 'object', 'properties': {}, 'additionalProperties': False}}]}
        elif method == 'resources/list':
            result = {'resources': [{'uri': RESOURCE, 'name': 'actual-resource', 'mimeType': 'text/plain'}]}
        elif method == 'resources/templates/list':
            result = {'resourceTemplates': []}
        elif method == 'resources/read':
            require(body['params']['uri'] == RESOURCE, 'resource target changed')
            result = {'contents': [{'uri': RESOURCE, 'mimeType': 'text/plain', 'text': UNTRUSTED}]}
        elif method == 'prompts/list':
            result = {'prompts': [{'name': 'actual-prompt', 'description': 'actual remote prompt'}]}
        elif method == 'prompts/get':
            require(body['params']['name'] == 'actual-prompt', 'prompt target changed')
            result = {'messages': [{'role': 'user', 'content': {'type': 'text', 'text': UNTRUSTED}}, {'role': 'assistant', 'content': {'type': 'text', 'text': UNTRUSTED + '_assistant'}}]}
        elif method == 'tools/call':
            callback_id = 'scoped-' + str(uuid.uuid4())
            callback = {'event': threading.Event(), 'reply': None, 'streamClosed': threading.Event()}
            with self.lock:
                self.callbacks[callback_id] = callback
            handler.send_response(200)
            handler.send_header('Content-Type', 'text/event-stream')
            handler.send_header('Connection', 'close')
            handler.end_headers()
            request = {'jsonrpc': '2.0', 'id': callback_id, 'method': 'sampling/createMessage', 'params': sample_request()}
            handler.wfile.write(('event: message\ndata: ' + json.dumps(request) + '\n\n').encode())
            handler.wfile.flush()
            handler.connection.settimeout(.1)
            deadline = time.monotonic() + 35
            while not callback['event'].is_set() or self.mode in ('cancel', 'eof', 'cancel-model'):
                require(time.monotonic() < deadline, 'native MCP reverse sampling callback did not settle')
                try:
                    if handler.connection.recv(1) == b'':
                        callback['streamClosed'].set()
                        handler.close_connection = True
                        return True
                except socket.timeout:
                    try:
                        handler.wfile.write(b': waiting\n\n')
                        handler.wfile.flush()
                    except OSError:
                        callback['streamClosed'].set()
                        handler.close_connection = True
                        return True
                except OSError:
                    callback['streamClosed'].set()
                    handler.close_connection = True
                    return True
            response = callback['reply']
            if self.mode in ('success', 'preapproved', 'two-roots') or (self.mode == 'budget' and 'result' in response):
                require(response.get('result') == {'role': 'assistant', 'model': 'fixture', 'content': {'type': 'text', 'text': OUTPUT}, 'stopReason': 'endTurn'}, 'reverse callback did not contain actual native model result')
                result = {'content': [{'type': 'text', 'text': MARKER}]}
            else:
                require('error' in response, 'denied/cancelled sampling returned success')
                result = {'isError': True, 'content': [{'type': 'text', 'text': 'SAMPLING_DENIED'}]}
            final = {'jsonrpc': '2.0', 'id': body['id'], 'result': result}
            handler.wfile.write(('event: message\ndata: ' + json.dumps(final) + '\n\n').encode())
            handler.wfile.flush()
            handler.close_connection = True
            return True
        else:
            raise AssertionError('unexpected MCP method ' + method)
        self.reply(handler, {'jsonrpc': '2.0', 'id': body['id'], 'result': result})
        return True


def run_case(args, root, version, mode):
    fixture = Fixture(mode)
    env, workspace = environment(root, fixture.server)
    policy = root / 'sampling-policy.json'
    dump(policy, {'resources': [{'resource': fixture.server.origin + '/mcp', 'cwd': str(workspace.resolve()), 'maxTokens': 1024, 'maxMessages': 16, 'maxDurationSeconds': 30, 'maxRequests': 2 if mode == 'budget' else 8, 'requireConsent': mode not in ('preapproved', 'cancel-model'), 'allowInsecureLoopback': True}]})
    if mode != 'default-off':
        env['RPI_ACP_MCP_SAMPLING_POLICY'] = str(policy.resolve())
    env['RPI_ACP_MCP_CATALOG'] = '1'
    bridge = None
    if mode == 'cancel-model':
        fixture.server.stalls[REMOTE] = {'received': threading.Event(), 'closed': threading.Event()}
    frames, handles = [], []
    result = {'version': version, 'mode': mode, 'status': 'fail'}
    try:
        bridge = Bridge(args.bridge, args.native, root / 'sessions', workspace, extra_env=env)
        caps = {'elicitation': {'form': {}}}
        init = {'protocolVersion': version, **({'clientCapabilities': caps} if version == 1 else {'info': {'name': 'native-sampling-acceptance', 'version': '1'}, 'capabilities': caps})}
        response, notes = bridge.request('initialize', init)
        frames.extend(notes + [response])
        require('result' in response, 'ACP initialize failed')
        roots = []
        for _ in range(3 if mode == 'budget' else (2 if mode == 'two-roots' else 1)):
            response, notes = bridge.request('session/new', {'cwd': str(workspace), 'mcpServers': [{'type': 'http', 'name': 'fixture', 'url': fixture.server.origin + '/mcp', 'headers': []}]})
            frames.extend(notes + [response])
            require('result' in response, 'actual native session/new failed: ' + json.dumps(response))
            roots.append(response['result']['sessionId'])
        require(fixture.initializations, 'MCP initialize absent')
        for init in fixture.initializations:
            require(init['protocolVersion'].startswith('2025-'), 'not initialized with 2025 MCP protocol')
            sampling = init.get('capabilities', {}).get('sampling')
            require((sampling == {}) if mode != 'default-off' else sampling is None, 'actual sampling capability does not reflect trusted policy')
        handles = native_handles(bridge.process.pid)
        require(len(handles) == len(roots), 'did not capture exactly one native process handle per root')
        requests = {}
        sent = 0
        for index, sid in enumerate(roots):
            ident = 700 + index
            requests[ident] = sid
            if mode == 'budget' and index > 0:
                continue
            sent += 1
            bridge.send({'jsonrpc': '2.0', 'id': ident, 'method': 'session/prompt', 'params': {'sessionId': sid, 'prompt': [{'type': 'text', 'text': CANARY + ' Use the fixture tool. Read remote catalog if available.'}]}})
        deadline = time.monotonic() + 55
        pending = set(requests)
        expected_output = 'CATALOG_NATIVE_OK' if mode == 'catalog' else ('TOOLS_ONLY_OK' if mode == 'tools-only' else 'ACP_NATIVE_FINAL')
        cancelled_ui = None
        while pending or (mode not in ('cancel', 'cancel-model') and (fixture.root_results < len(roots) or expected_output not in json.dumps(frames))) or (mode in ('cancel', 'cancel-model') and not cancelled_ui):
            if mode == 'budget' and fixture.root_results >= sent and sent < len(roots):
                bridge.send({'jsonrpc': '2.0', 'id': 700 + sent, 'method': 'session/prompt', 'params': {'sessionId': roots[sent], 'prompt': [{'type': 'text', 'text': CANARY + ' Use the fixture tool.'}]}})
                sent += 1
            if mode == 'cancel-model' and fixture.server.stalls[REMOTE]['received'].is_set() and cancelled_ui is None:
                bridge.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': roots[0]}})
                cancelled_ui = True
            try:
                frame = bridge.output.get(timeout=min(.1, max(.01, deadline - time.monotonic())) )
            except queue.Empty:
                require(time.monotonic() < deadline, 'parent root/callback deadlock')
                continue
            frames.append(frame)
            require('__decode_error' not in frame, 'ACP stdout was not JSON')
            if frame.get('method') == 'elicitation/create':
                fixture.consent_count += 1
                require(mode not in ('preapproved', 'cancel-model'), 'admin preapproval unexpectedly requested UI')
                params = frame['params']
                require(params.get('sessionId') in roots, 'consent attached to wrong native root')
                require(params.get('mode', 'form') == 'form', 'sampling consent not form mode')
                schema = params['requestedSchema']
                require(schema['properties']['approved']['type'] == 'boolean', 'consent schema missing boolean approved')
                if mode in ('cancel', 'eof'):
                    if mode == 'eof':
                        bridge.process.stdin.close()
                        bridge.process.wait(timeout=15)
                        pending.clear()
                        break
                    cancelled_ui = False
                    bridge.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': params['sessionId']}})
                else:
                    bridge.send({'jsonrpc': '2.0', 'id': frame['id'], 'result': {'action': 'decline' if mode == 'decline' else 'accept', 'content': {'approved': mode != 'decline'}}})
            if mode == 'cancel' and frame.get('method') == '$/cancel_request':
                require(any(f.get('method') == 'elicitation/create' and f['id'] == frame['params']['requestId'] for f in frames), 'wrong consent cancellation correlation')
                cancelled_ui = True
            if frame.get('id') in pending and 'method' not in frame:
                require('result' in frame, 'ACP prompt failed: ' + json.dumps(frame))
                pending.remove(frame['id'])
            if time.monotonic() >= deadline:
                raise AssertionError('parent root/callback deadlock')
        if mode == 'budget':
            require(fixture.sampling_calls == 2 and fixture.consent_count == 2, 'writer-global budget did not limit three roots to two model calls/forms')
            require(len(fixture.callbacks) == len(fixture.replies) == 3, 'budget did not execute three real tool callbacks')
            require(all('result' in r for r in fixture.replies[:2]) and 'error' in fixture.replies[2], 'third shared-budget callback not denied')
            require(fixture.replies[2]['error'].get('code') == -32603 and fixture.replies[2]['error'].get('message') == 'Sampling denied', 'budget denial fixed error changed')
        if mode in ('cancel', 'eof', 'cancel-model'):
            if mode == 'cancel-model':
                require(fixture.sampling_calls == 1 and cancelled_ui, 'isolated generation cancellation not exercised')
                require(fixture.server.stalls[REMOTE]['closed'].wait(5), 'cancel left isolated model socket alive')
            require(len(fixture.callbacks) == 1, 'cancel did not invoke actual MCP tool')
            for callback in fixture.callbacks.values():
                require(callback['streamClosed'].wait(5), 'cancel/EOF left reverse HTTP stream alive')
            replies_at_retirement = len(fixture.replies)
            time.sleep(.3)
            require(len(fixture.replies) == replies_at_retirement, 'late reverse callback reply after stream retirement')
            require(all('error' in reply for reply in fixture.replies), 'cancelled callback returned successful sampling')
            result['reverseStreamsClosed'] = sum(c['streamClosed'].is_set() for c in fixture.callbacks.values())
            result['noLateReverseReply'] = True
            if mode == 'cancel-model':
                result['isolatedModelStreamClosed'] = fixture.server.stalls[REMOTE]['closed'].is_set()
        if mode in ('success', 'two-roots', 'preapproved'):
            require(fixture.sampling_calls == len(roots), 'actual isolated model call count differs from roots')
            require(len(fixture.replies) == len(roots), 'reverse response/root isolation mismatch')
            require(fixture.consent_count == (0 if mode == 'preapproved' else len(roots)), 'consent reused across roots')
            require('ACP_NATIVE_FINAL' in json.dumps(frames), 'actual parent model final output absent from ACP')
        elif mode in ('decline', 'default-off', 'cancel', 'eof'):
            require(fixture.sampling_calls == 0, 'denied/cancelled/off sampling reached model')
        if mode == 'catalog':
            require(all(method in fixture.events for method in ('resources/list', 'resources/read', 'prompts/list', 'prompts/get')), 'catalog operations not actually executed')
            require('CATALOG_NATIVE_OK' in json.dumps(frames), 'catalog output absent from ACP')
        require(not fixture.server.failures, 'fixture assertion failures: ' + str(fixture.server.failures))
        result.update(status='pass', samplingModelCalls=fixture.sampling_calls, nativeParentCompletions=fixture.root_results, reverseReplies=len(fixture.replies), consentForms=fixture.consent_count)
    except Exception as exc:
        result['error'] = (str(exc) or type(exc).__name__).replace(KEY, '[REDACTED]')
    finally:
        if bridge:
            try:
                bridge.close()
            except Exception as exc:
                result.update(status='fail', cleanupError=str(exc))
            try:
                result['nativeExitHandles'] = wait_native_handles(handles)
            except Exception as exc:
                result.update(status='fail', nativeExitError=str(exc))
            # The native process owns exclusive Windows lock/control files.
            # Inspect every remaining artifact only after its exit, not while
            # those handles are intentionally locked. Do not skip read errors.
            try:
                for path in (root / 'sessions').rglob('*'):
                    if path.is_file():
                        require(KEY.encode() not in path.read_bytes(), 'model credential persisted in session artifacts')
            except Exception as exc:
                result.update(status='fail', artifactError=str(exc).replace(KEY, '[REDACTED]'))
            (root / 'bridge-stderr.log').write_text(''.join(bridge.errors).replace(KEY, '[REDACTED]'), encoding='utf-8')
        dump(root / 'frames.json', frames)
        dump(root / 'model-requests.json', fixture.model_bodies)
        dump(root / 'mcp.json', {'methods': fixture.events, 'initialize': fixture.initializations, 'reverseReplies': fixture.replies, 'streamClosures': {ident: callback['streamClosed'].is_set() for ident, callback in fixture.callbacks.items()}, 'fixtureFailures': fixture.server.failures})
        fixture.server.shutdown()
        fixture.server.server_close()
        dump(root / 'results.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--expected-sha256', default=PIN, help='Explicit trusted native SHA256 (default: original pinned binary)')
    parser.add_argument('--bridge', type=Path)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--modes', nargs='+', choices=['success', 'preapproved', 'decline', 'default-off', 'two-roots', 'catalog', 'tools-only', 'cancel', 'eof', 'budget', 'cancel-model'], default=['default-off', 'success', 'preapproved', 'decline', 'two-roots', 'catalog', 'tools-only', 'cancel', 'eof', 'budget', 'cancel-model'])
    args = parser.parse_args()
    args.native = args.native.resolve()
    if args.bridge:
        args.bridge = args.bridge.resolve()
    root = args.artifacts.resolve()
    require(root.drive.upper() == 'C:' and root.name.startswith('acp-sampling-native-run-'), 'use C: acp-sampling-native-run-* artifact directory')
    root.mkdir(parents=True, exist_ok=False)
    report = {'checks': [], 'expectedSha256': args.expected_sha256, 'pendingCoverage': ['writer-global request budget across three roots', 'native process liveness handles on ACP EOF', 'reverse stream socket closure at parent cancellation', 'isolated model stream cancellation']}
    if not args.native.is_file():
        report['checks'].append({'status': 'blocked', 'reason': 'pinned native binary missing'})
    elif not args.bridge or not args.bridge.is_file():
        report['checks'].append({'status': 'blocked', 'reason': 'final parent bridge rebuild required; --bridge omitted or absent'})
    else:
        report['nativeSha256'] = hashlib.sha256(args.native.read_bytes()).hexdigest()
        report['bridgeSha256'] = hashlib.sha256(args.bridge.read_bytes()).hexdigest()
        if report['nativeSha256'] != args.expected_sha256:
            report['checks'].append({'status': 'blocked', 'reason': 'pinned native SHA256 mismatch'})
        else:
            for version in (1, 2):
                for mode in args.modes:
                    report['checks'].append(run_case(args, root / ('v' + str(version) + '-' + mode), version, mode))
            for coverage, modes in [('writer-global request budget across three roots', ['budget']), ('native process liveness handles on ACP EOF', ['eof']), ('reverse stream socket closure at parent cancellation', ['cancel', 'eof']), ('isolated model stream cancellation', ['cancel-model'])]:
                if all(any(c.get('version') == v and c.get('mode') == m and c['status'] == 'pass' for c in report['checks']) for v in (1, 2) for m in modes):
                    report['pendingCoverage'].remove(coverage)
    report['counts'] = {status: sum(c['status'] == status for c in report['checks']) for status in ('pass', 'fail', 'blocked')}
    dump(root / 'results.json', report)
    print(json.dumps(report, indent=2))
    return 1 if report['counts']['fail'] or report['counts']['blocked'] or report['pendingCoverage'] else 0


if __name__ == '__main__':
    raise SystemExit(main())
