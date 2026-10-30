#!/usr/bin/env python3
"""Actual native ChildAgent tools/interactive identity acceptance (stdlib only).

Build/freeze inputs separately; this script NEVER builds or installs anything.
Only the loopback OpenAI provider is fake: delegate_task, child creation,
read/write/bash, approval hooks and ask_user all execute in the real native host.
The ACP client serves in-memory unsaved buffers/terminals and validated forms.

python child_tools_native.py --bridge ACP_EXE --native RPI_EXE \
  --plugin SUBAGENTS_DLL --ask-plugin ASK_DLL --gate-plugin GATE_DLL \
  --artifacts EMPTY_C_DIRECTORY [--wait-native 120]

Every announced child's private _meta association must equal the copied actual
outgoing parent model tool_call ID. Host-returned parent assistant tool_calls
are also captured for completed runs. Cancel/EOF use the emitted call plus the
native parent ACP tool card, because no parent model continuation is expected.
Untrusted context always includes a forged ID; real producer arguments include
it too unless the advertised schema explicitly disallows that property.

Missing optional plugin arguments are explicit FAILED coverage, not skips.
Every case retains frames, model inputs, isolation metadata and EOF cleanup.
An EOF case requires observable child-scoped kill/release, not just process exit.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import time
import traceback
import uuid

from subagents_native import (Model, Peer, OPT_IN, REVISION, check, dump,
                              environment, strings)

TEMP = Path('C:/Users/bigfish/.cargo/target/acp-tmp')


def tool_call(call_id, name, arguments):
    return {'tool_calls': [{'index': 0, 'id': call_id, 'type': 'function',
        'function': {'name': name, 'arguments': json.dumps(arguments)}}]}, 'tool_calls'


class ChildModel(Model):
    def __init__(self, args, fixture):
        self.fixture = fixture
        super().__init__(args)

    def respond(self, body):
        return self.fixture.model(body)


class Acceptance:
    def __init__(self, root, mode, version):
        self.root, self.mode, self.version = root, mode, version
        self.frames, self.requests = [], []
        self.roots, self.children, self.announcements = {}, {}, {}
        self.boot_roots = set()
        self.terminals = {}
        self.active = None
        self.triggered = False
        self.eof_sent = False
        self.peer = None
        self.provenance = {}

    def add_root(self, sid, index):
        marker = f'{index}_{uuid.uuid4().hex}'
        workspace = self.root / 'workspace'
        read = workspace / f'{marker}-unsaved.txt'
        read.write_text('LOCAL_DECOY_NEVER_RETURN', encoding='utf-8')
        d = {'sid': sid, 'marker': marker, 'read': str(read),
             'mode': 'normal' if self.mode == 'cancel' and index == 1 else self.mode,
             'write': str(workspace / f'{marker}-write.txt'),
             'denied': str(workspace / f'{marker}-denied.txt'),
             'input': 'CHILD_CLIENT_READ_' + marker,
             'written': 'CHILD_CLIENT_WRITE_' + marker,
             'root_input': 'ROOT_CLIENT_READ_' + marker,
             'output': 'CHILD_CLIENT_TERMINAL_' + marker,
             'wait_error': 'CHILD_CLIENT_WAIT_ERROR_' + marker,
             'denial': 'CHILD_CLIENT_EXPLICIT_DENY_' + marker,
             'answer': 'VALIDATED_ANSWER_' + marker,
             'child_final': 'CHILD_FINAL_' + marker,
             'parent_final': 'PARENT_FINAL_' + marker,
             'child_stages': set(), 'parent_stages': set(), 'permissions': [],
             'forms': 0, 'buffers': {str(read): 'CHILD_CLIENT_READ_' + marker}}
        self.roots[sid] = d
        self.boot_roots.add(sid)
        return d

    def steps(self, d):
        if d['mode'] in ('cancel', 'eof'):
            return [('bash', {'command': 'echo child-pending-' + d['marker'], 'timeout': 60}, None)]
        if d['mode'] == 'deny':
            return [('read', {'path': d['read']}, d['denial']),
                    ('write', {'path': d['denied'], 'content': 'MUST_NOT_WRITE'}, 'blocked')]
        return [('read', {'path': d['read']}, d['input']),
                ('write', {'path': d['write'], 'content': d['written']}, 'Successfully wrote'),
                ('read', {'path': d['write']}, d['written']),
                ('bash', {'command': 'echo child-success-' + d['marker'], 'timeout': 10}, d['output']),
                ('bash', {'command': 'echo child-error-' + d['marker'], 'timeout': 10}, d['wait_error']),
                ('ask_user', {'question': 'Choose child identity ' + d['marker'],
                              'options': [d['answer'], 'WRONG_ANSWER'],
                              'allowFreeform': False, 'timeout': 20}, d['answer'])]

    def model(self, body):
        users = [m for m in body['messages'] if m.get('role') == 'user']
        check(users, ('missing actual user input', body))
        latest = strings(users[-1])
        matches = [d for d in self.roots.values() if d['marker'] in latest]
        check(len(matches) == 1, ('cannot identify actual model request', latest))
        d = matches[0]
        # Capture host-returned assistant calls, not a value from ACP metadata.
        evidence = self.provenance.setdefault(d['sid'], {
            'marker': d['marker'], 'emitted': None, 'parent_inputs': [],
            'parent_cards': [], 'associations': [],
            'spoof': 'FORGED_PARENT_' + d['marker']})
        child = 'CHILD_TOOLS_' + d['marker'] in latest
        if not child:
            for message in body['messages']:
                if message.get('role') == 'assistant':
                    for call in message.get('tool_calls', []):
                        if call.get('function', {}).get('name') == 'delegate_task':
                            check(evidence['emitted'] is not None, ('unissued parent call', call))
                            check(call['id'] == evidence['emitted']['id'], ('host changed parent call ID', call))
                            arguments = json.loads(call['function']['arguments'])
                            check('CHILD_TOOLS_' + d['marker'] in arguments['task'],
                                  ('parent assistant call lost fixture marker', call))
                            evidence['parent_inputs'].append(json.loads(json.dumps(call)))
        results = {m.get('tool_call_id'): m for m in body['messages'] if m.get('role') == 'tool'}
        names = {t['function']['name'] for t in body.get('tools', [])}
        if child:
            # Never select delegate_task in a child, even if an old host advertises it.
            steps = self.steps(d)
            stage = 0
            while f"child-{d['marker']}-{stage}" in results:
                cid = f"child-{d['marker']}-{stage}"
                check(stage < len(steps), ('unexpected child result', cid))
                expected = steps[stage][2]
                actual = strings(results[cid])
                check(expected is not None and expected.lower() in actual.lower(),
                      ('actual client/plugin result missing from CHILD model loop', stage, expected, actual))
                check('LOCAL_DECOY_NEVER_RETURN' not in actual, ('local fallback', results[cid]))
                for other in self.roots.values():
                    if other is not d:
                        check(other['marker'] not in actual, ('cross-root child result leak', actual))
                d['child_stages'].add(stage)
                stage += 1
            if stage == len(steps):
                return {'content': d['child_final']}, 'stop'
            name, arguments, _ = steps[stage]
            check(name in names, ('real child tool not advertised', name, sorted(names)))
            return tool_call(f"child-{d['marker']}-{stage}", name, arguments)
        root_call = 'root-read-' + d['marker']
        delegate = 'delegate-' + d['marker']
        if root_call not in results:
            check('read' in names, names)
            return tool_call(root_call, 'read', {'path': d['read']})
        check(d['root_input'] in strings(results[root_call]), ('parent client read not returned', results))
        d['parent_stages'].add('read')
        if delegate not in results:
            check('delegate_task' in names, ('actual rpi-subagents DLL missing', names))
            producer = next(t['function'] for t in body['tools']
                            if t['function']['name'] == 'delegate_task')
            schema = producer.get('parameters', {})
            permits_spoof = (schema.get('additionalProperties') is not False
                             or 'parentToolCallId' in schema.get('properties', {}))
            arguments = {'task': 'CHILD_TOOLS_' + d['marker'] +
                ': run the requested native tool sequence, then return your final response. Never delegate.',
                'role': 'child-tools-acceptance', 'maxTurns': 16, 'timeoutSeconds': 90}
            if permits_spoof:
                arguments['parentToolCallId'] = evidence['spoof']
            evidence['argument_spoof'] = ('attempted' if permits_spoof else
                'skipped: advertised schema forbids unknown parentToolCallId')
            evidence['producer_schema'] = schema
            response = tool_call(delegate, 'delegate_task', arguments)
            # Copy the actual outgoing model tool_call before native executes it.
            # Never derive this expected value from the child's association.
            emitted = json.loads(json.dumps(response[0]['tool_calls'][0]))
            check(evidence['emitted'] in (None, emitted), ('parent call changed on retry', evidence))
            evidence['emitted'] = emitted
            return response
        check(d['child_final'] in strings(results[delegate]),
              ('actual child final not returned through real delegate_task to PARENT model', results[delegate]))
        d['parent_stages'].add('delegate')
        return {'content': d['parent_final']}, 'stop'

    def observe(self, frame):
        self.frames.append(frame)
        check('invalid_stdout' not in frame and not frame.get('eof'), ('closed/invalid ACP', frame))
        if frame.get('method') != 'session/update':
            return
        p = frame['params']
        sid, update = p['sessionId'], p['update']
        check(sid in self.roots or sid in self.children, ('unannounced/cross-root update', frame))
        owner = sid if sid in self.roots else self.children[sid]
        if sid in self.boot_roots and update['sessionUpdate'] == 'available_commands_update':
            return  # Root-only boot metadata, including queued post-response updates.
        check(owner == self.active, ('traffic crossed active root', frame, self.active))
        d = self.roots[owner]
        if update['sessionUpdate'] in ('tool_call', 'tool_call_update'):
            call_id = update.get('toolCallId', '')
            if call_id.startswith('child-' + d['marker'] + '-'):
                check(sid in self.children, ('child tool card routed to root UI', frame))
            elif call_id in ('root-read-' + d['marker'], 'delegate-' + d['marker']):
                check(sid == owner, ('parent tool card routed to child UI', frame))
                evidence = self.provenance.get(owner)
                if evidence and evidence['emitted'] and call_id == evidence['emitted']['id']:
                    evidence['parent_cards'].append(len(self.frames) - 1)
            else:
                raise AssertionError(('unknown/cross-root tool identity', frame))
        if update['sessionUpdate'] == 'subagent_update':
            cid = update['sessionId']
            check(sid in self.roots, ('announcement not attached to root', frame))
            check(cid.startswith(f'child:{len(sid)}:{sid}:'), ('unscoped child ID', frame))
            evidence = self.provenance.get(sid)
            check(evidence and evidence['emitted'], ('association without captured model tool_call', frame))
            check('parentToolCallId' not in update and 'parentToolCallId' not in p,
                  ('private association leaked into standard ACP field', frame))
            actual = update.get('_meta', {}).get('rpi/parentToolCallId')
            expected = evidence['emitted']['id']
            check(actual == expected and actual != evidence['spoof'],
                  ('native parent provenance mismatch', expected, actual, frame))
            check(evidence['parent_cards'], ('child announced before actual parent tool card', frame))
            evidence['associations'].append({'frame': len(self.frames) - 1, 'child': cid,
                                             'actual': actual, 'expected': expected})
            if cid not in self.children:
                check(update.get('capabilities') == {}, ('missing real announcement', frame))
                check(sid not in self.children.values(), ('more than one child per root', frame))
                self.children[cid] = sid
                self.announcements[cid] = len(self.frames) - 1
            check(self.children[cid] == sid, ('child ID reused across roots', frame))

    def respond(self, frame):
        self.requests.append(frame)
        method, p = frame['method'], frame['params']
        sid = p.get('sessionId')
        root_request = sid in self.roots
        check(root_request or sid in self.children, ('interaction before actual child announcement', frame))
        owner = sid if root_request else self.children[sid]
        check(owner == self.active, ('cross-root interaction', frame, self.active))
        d = self.roots[owner]
        mode = d['mode']
        if root_request:
            check(method == 'fs/read_text_file' and p.get('path') == d['read'],
                  ('child interaction incorrectly routed to parent', frame))
            return {'content': d['root_input']}, None
        check(self.announcements[sid] < len(self.frames) - 1, ('announcement not BEFORE request', frame))
        if method.startswith('fs/'):
            path = p['path']
            check(Path(path).is_absolute(), ('relative client path', frame))
            if method == 'fs/read_text_file':
                check(path in d['buffers'], ('unexpected child file', frame))
                if mode == 'deny':
                    return None, {'code': -32001, 'message': d['denial']}
                return {'content': d['buffers'][path]}, None
            if method == 'fs/write_text_file':
                check(mode != 'deny', ('denied tool reached client filesystem', frame))
                check(path == d['write'] and p['content'] == d['written'], frame)
                d['buffers'][path] = p['content']
                return {}, None
        if method == 'session/request_permission':
            if self.version == 2:
                check('toolCall' not in p and p.get('title'), ('invalid v2 permission shape', frame))
                check(p.get('subject', {}).get('type') == 'tool_call', frame)
                tool_call = p['subject']['toolCall']
            else:
                tool_call = p['toolCall']
            check(tool_call['toolCallId'].startswith('child-' + d['marker'] + '-'),
                  ('permission not correlated to actual child tool call', frame))
            wanted = 'reject_once' if mode == 'deny' else 'allow_once'
            options = [o for o in p['options'] if o['kind'] == wanted]
            check(len(options) == 1, ('actual permission options missing', frame))
            d['permissions'].append(wanted)
            return {'outcome': {'outcome': 'selected', 'optionId': options[0]['optionId']}}, None
        if method == 'elicitation/create':
            check(mode == 'normal' and p['mode'] == 'form', frame)
            check(p['toolCallId'] == 'child-' + d['marker'] + '-5', frame)
            schema = p['requestedSchema']
            check(schema.get('type') == 'object' and schema['required'] == ['answer'], frame)
            prop = schema['properties']['answer']
            check(prop['type'] == 'string' and prop['enum'] == [d['answer'], 'WRONG_ANSWER'], frame)
            d['forms'] += 1
            return {'action': 'accept', 'content': {'answer': d['answer']}}, None
        if method == 'terminal/create':
            check(mode != 'deny', ('denied child reached terminal', frame))
            check(p['command'] == 'bash' and Path(p['cwd']).resolve() == (self.root / 'workspace').resolve(), frame)
            args = p['args']
            check(len(args) == 2 and args[0] == '-c' and args[1].startswith('echo child-')
                  and args[1].endswith(d['marker']), frame)
            tid = 'client-terminal-' + uuid.uuid4().hex
            self.terminals[tid] = {'sid': sid, 'mode': mode, 'methods': [method], 'error': 'child-error-' in args[1]}
            return {'terminalId': tid}, None
        if method.startswith('terminal/'):
            t = self.terminals.get(p.get('terminalId'))
            check(t and t['sid'] == sid, ('terminal owner changed (including cleanup)', frame, t))
            t['methods'].append(method)
            if method == 'terminal/wait_for_exit':
                if mode in ('cancel', 'eof'):
                    check(not self.triggered, ('repeated pending wait', frame))
                    self.triggered = True
                    if mode == 'cancel':
                        self.peer.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': owner}})
                    else:
                        self.peer.p.stdin.close()
                        self.eof_sent = True
                    return None  # Leave actual client request outstanding.
                if t['error']:
                    return None, {'code': -32603, 'message': d['wait_error']}
                return {'exitCode': 0}, None
            if method == 'terminal/output':
                check(not t['error'], ('output after terminal error', frame))
                return {'output': d['output'], 'truncated': False, 'exitStatus': {'exitCode': 0}}, None
            if method in ('terminal/kill', 'terminal/release'):
                return {}, None
        raise AssertionError(('unexpected actual client request', frame))

    def verify(self):
        check(len(self.children) == len(self.roots), ('missing child announcements', self.children))
        check(len(set(self.children)) == len(self.roots), ('two-root child isolation', self.children))
        captured_ids = [p['emitted']['id'] for p in self.provenance.values() if p['emitted']]
        check(len(captured_ids) == len(self.roots) and len(set(captured_ids)) == len(self.roots),
              ('root A/B captured parent IDs not distinct', captured_ids))
        for sid, d in self.roots.items():
            evidence = self.provenance[sid]
            check(evidence['associations'], ('missing exact parent association', evidence))
            if d['mode'] in ('normal', 'deny'):
                check(evidence['parent_inputs'], ('no parent input assistant tool_call captured', evidence))
            check(Path(d['read']).read_text(encoding='utf-8') == 'LOCAL_DECOY_NEVER_RETURN', 'read decoy changed')
            check(not Path(d['write']).exists() and not Path(d['denied']).exists(), 'local write fallback')
            if d['mode'] in ('normal', 'deny'):
                check(d['child_stages'] == set(range(len(self.steps(d)))), ('unvalidated child tool stages', d))
                check(d['parent_stages'] == {'read', 'delegate'}, ('missing real parent continuation', d))
                cid = next(c for c, owner in self.children.items() if owner == sid)
                for target, expected in ((sid, d['parent_final']), (cid, d['child_final'])):
                    actual = ''.join(f['params']['update'].get('content', {}).get('text', '')
                        for f in self.frames if f.get('method') == 'session/update'
                        and f['params']['sessionId'] == target
                        and f['params']['update']['sessionUpdate'] == 'agent_message_chunk')
                    check(actual == expected, ('final text missing/duplicated/wrong identity', target, expected, actual))
                check(d['permissions'] == (['allow_once'] * 3 if d['mode'] == 'normal' else ['reject_once']),
                      ('real gate allow/deny coverage missing', d['permissions']))
                check(d['forms'] == (1 if d['mode'] == 'normal' else 0), ('real ask_user form not proven', d))
            else:
                check(self.triggered, ('cancellation/EOF never reached actual child terminal', self.mode))
        for t in self.terminals.values():
            expected = ['terminal/create', 'terminal/wait_for_exit']
            if t['mode'] == 'normal' and not t['error']:
                expected += ['terminal/output', 'terminal/release']
            else:
                expected += ['terminal/kill', 'terminal/release']
            check(t['methods'] == expected, ('terminal lifecycle/cleanup not proven', self.mode, expected, t))
        expected_count = sum(2 if d['mode'] == 'normal' else
                             1 if d['mode'] in ('cancel', 'eof') else 0 for d in self.roots.values())
        check(len(self.terminals) == expected_count, ('terminal count', self.terminals))
        return {'roots': list(self.roots), 'children': self.children,
                'terminal_lifecycles': self.terminals, 'requests': len(self.requests),
                'parent_provenance': self.provenance,
                'validated_child_stages': {sid: sorted(d['child_stages']) for sid, d in self.roots.items()}}


class Client(Peer):
    def __init__(self, command, env, root, fixture):
        super().__init__(command, env, root)
        self.fixture = fixture
        self.new_pending = None
        fixture.peer = self

    def new_root(self, params, index):
        check(self.new_pending is None, 'overlapping session/new')
        self.new_pending = []
        try:
            result = self.request('session/new', params)
            sid = result['sessionId']
            check(isinstance(sid, str) and sid and not sid.startswith('child:')
                  and sid not in self.fixture.roots and sid not in self.fixture.children,
                  ('invalid/duplicate new root identity', result))
            check(all(f['params']['sessionId'] == sid for f in self.new_pending),
                  ('provisional updates disagree with exact session/new response', result, self.new_pending))
            self.fixture.add_root(sid, index)
            for frame in self.new_pending:
                self.fixture.observe(frame)
            return sid
        finally:
            self.new_pending = None

    def receive(self, deadline):
        frame = self.q.get(timeout=max(.01, deadline - time.monotonic()))
        self.frames.append(frame)
        if (self.new_pending is not None and frame.get('method') == 'session/update'
                and frame.get('params', {}).get('sessionId') not in self.fixture.roots):
            p = frame['params']
            sid = p['sessionId']
            check('id' not in frame and isinstance(sid, str) and sid and not sid.startswith('child:')
                  and sid not in self.fixture.children
                  and p['update']['sessionUpdate'] == 'available_commands_update',
                  ('not provisional root boot metadata', frame))
            check(len(self.new_pending) < 32 and len(json.dumps(frame)) <= 65536,
                  ('provisional root boot updates exceeded bound', frame))
            check(not self.new_pending or self.new_pending[0]['params']['sessionId'] == sid,
                  ('multiple provisional roots', frame))
            self.new_pending.append(frame)
            return frame
        self.fixture.observe(frame)
        if frame.get('method') and 'id' in frame:
            response = self.fixture.respond(frame)
            if response is not None and not self.fixture.eof_sent:
                result, error = response
                self.send({'jsonrpc': '2.0', 'id': frame['id'],
                           **({'error': error} if error else {'result': result})})
        return frame


def closed_jsonl(path):
    # Native JSONL files are exclusively held on Windows. Call ONLY after
    # Peer.close has verified bridge and retained native process handles exited.
    check(path.stat().st_size <= 8 * 1024 * 1024, ('oversized fixture transcript', str(path)))
    raw = path.read_bytes()  # PermissionError is a failure, never a coverage skip.
    lines = raw.splitlines()
    check(0 < len(lines) <= 4096, ('unbounded/empty fixture transcript', str(path)))
    rows = [json.loads(line) for line in lines]
    check(rows[0].get('kind') == 'header', ('missing native header', str(path)))
    ids = [r['id'] for r in rows[1:]]
    check(len(ids) == len(set(ids)), ('duplicate native records', str(path)))
    messages = [r['message'] for r in rows if r.get('kind') == 'entry'
                and r.get('type') == 'message']
    return raw, rows, messages


def expected_rpc_messages(messages):
    """Project only the observed native serde representation difference.

    Native rpi-ai/src/types.rs ToolResultMessage.details is Option<Value> with
    default + skip_serializing_if=Option::is_none: persisted null deserializes
    to None, which rpc_get_messages omits when serializing entry.message.
    Do not normalize actual RPC data or any substantive/unknown field.
    """
    expected = json.loads(json.dumps(messages))
    for message in expected:
        if (message.get('kind') == 'tool_result'
                and message.get('role') == 'toolResult'
                and 'details' in message and message['details'] is None):
            del message['details']
    return expected


def message_calls(messages):
    return [c for m in messages if m.get('role') == 'assistant'
            for c in m.get('content', []) if c.get('type') == 'toolCall']


def verify_closed_storage(args, f, env, model):
    evidence = []
    dump(f.root / 'durable-storage.json', evidence)
    root_files = [p for p in (f.root / 'sessions').rglob('*.jsonl')
                  if '.children' not in p.parts]
    check(len(root_files) == len(f.roots), ('actual root transcript count', root_files))
    for alias, d in f.roots.items():
        candidates = [p for p in root_files if alias in p.name]
        check(len(candidates) == 1, ('actual ACP root native path', alias, candidates))
        root_path = candidates[0]
        root_bytes, root_rows, root_messages = closed_jsonl(root_path)
        root_id = root_rows[0]['id']
        check(root_id == alias, ('ACP root/native header mismatch', alias, root_rows[0]))
        child_alias = next(c for c, owner in f.children.items() if owner == alias)
        prefix = f'child:{len(alias)}:{alias}:'
        native_child = child_alias.removeprefix(prefix)
        check(child_alias.startswith(prefix) and str(uuid.UUID(native_child)) == native_child,
              ('ACP scoped alias/native UUID', child_alias))
        child_area = root_path.parent / '.children' / root_id
        children = list(child_area.rglob('*.jsonl'))
        check(len(children) == 1, ('actual nested child JSONL count', str(child_area), children))
        child_path = children[0]
        check(child_path.parent.parent == child_area and
              child_path.parent.name == root_path.parent.name,
              ('native repository cwd layout not retained', str(root_path), str(child_path)))
        child_bytes, rows, messages = closed_jsonl(child_path)
        header = rows[0]
        check(header['id'] == native_child and header['parentSessionId'] == root_id,
              ('native child/header parent identity', header, child_alias))
        check(header['cwd'] == root_rows[0]['cwd'], ('child cwd changed', header))
        relation = header.get('metadata', {})  # Native raw header, NOT ACP _meta.
        expected_parent = f.provenance[alias]['emitted']['id']
        check(relation == {'kind': 'delegated-child', 'version': 1,
                           'rootSessionId': root_id, 'parentToolCallId': expected_parent},
              ('native trusted relation metadata', relation, expected_parent))
        check(relation['parentToolCallId'] != f.provenance[alias]['spoof'], relation)
        users = [m for m in messages if m.get('role') == 'user']
        check(len(users) == 1 and 'CHILD_TOOLS_' + d['marker'] in strings(users[0]),
              ('child own persisted prompt missing/duplicated', users))
        check(not any(r.get('type') == 'assistant_progress' or
                      (r.get('type') == 'assistant_frame' and r.get('op') != 'commit_stream')
                      for r in rows), ('volatile child progress persisted', str(child_path)))
        # A commit_stream is a structural entry-ID reference, not a progress
        # payload or a second transcript. Reject any embedded message content.
        assistant_entries = {r['id'] for r in rows if r.get('kind') == 'entry'
                             and r.get('message', {}).get('role') == 'assistant'}
        for record in rows:
            if record.get('type') == 'assistant_frame':
                check(set(record.get('frame', {})) == {'entryId'} and
                      record['frame']['entryId'] in assistant_entries,
                      ('duplicated/uncommitted child frame payload', record))
        calls = message_calls(messages)
        expected_calls = [(f"child-{d['marker']}-{i}", step[0])
                          for i, step in enumerate(f.steps(d))]
        check([(c['id'], c['name']) for c in calls] == expected_calls,
              ('child canonical assistant calls missing/duplicated', calls, expected_calls))
        results = [m for m in messages if m.get('role') == 'toolResult']
        if d['mode'] in ('normal', 'deny'):
            check([(m['toolCallId'], m['toolName']) for m in results] == expected_calls,
                  ('child canonical tool results missing/duplicated', results))
            for m, step in zip(results, f.steps(d)):
                check(step[2].lower() in strings(m).lower(), ('persisted child tool output', step, m))
            finals = [m for m in messages if m.get('role') == 'assistant'
                      and d['child_final'] in strings(m)]
            check(len(finals) == 1, ('child final missing/duplicated', finals))
            check(all(m.get('provider') == 'child-local' and m.get('model') == 'fixture'
                      for m in messages if m.get('role') == 'assistant'),
                  ('persisted child provider/model changed', messages))
        # Inspect canonical role boundaries, not arbitrary substrings: a root
        # delegate result may legitimately contain child text, IDs and summaries.
        check(all(not c['id'].startswith('child-') for c in message_calls(root_messages)),
              ('child assistant calls contaminated root', str(root_path)))
        check(all(not m.get('toolCallId', '').startswith('child-') for m in root_messages
                  if m.get('role') == 'toolResult'), ('child tool results contaminated root', str(root_path)))
        check(len([m for m in root_messages if m.get('role') == 'user']) == 1,
              ('child user prompt contaminated root', str(root_path)))
        check(not any(d['child_final'] in strings(m) for m in root_messages
                      if m.get('role') == 'assistant'), ('child assistant final contaminated root', str(root_path)))
        item = {'root_alias': alias, 'root_native_id': root_id, 'root_path': str(root_path),
                'child_alias': child_alias, 'child_native_id': native_child,
                'child_path': str(child_path), 'header': header,
                'root_sha256': hashlib.sha256(root_bytes).hexdigest(),
                'child_sha256': hashlib.sha256(child_bytes).hexdigest(),
                'message_count': len(messages), 'tool_calls': expected_calls}
        evidence.append(item)
        dump(f.root / 'durable-storage.json', evidence)
        if d['mode'] == 'normal':
            # Only settled normal children: CLI recovery of interrupted jobs is
            # deliberately not part of this idle read-only acceptance.
            reopen_root = f.root / ('reopen-' + native_child)
            (reopen_root / 'workspace').mkdir(parents=True)
            # Native --session resolves within --session-dir, even for a path.
            # Select the validated child repository explicitly; never fall back
            # to the fresh reopen workspace's default .rpi/sessions directory.
            command = [str(args.native), '--session-dir', str(child_area),
                       '--session', native_child, '--mode', 'rpc', '--no-extensions']
            item['reopen_command'] = command
            before_calls = len(model.captures)
            reader = None
            try:
                reader = Peer(command, env, reopen_root)
                ready = reader.receive(time.monotonic() + 20)
                check(ready.get('type') == 'ready' and ready.get('protocol') == 'rpi', ready)
                reader.send({'type': 'get_messages', 'id': 'durable-read', 'limit': 64})
                deadline = time.monotonic() + 20
                while True:
                    frame = reader.receive(deadline)
                    check(frame.get('type') not in ('agent_start', 'tool_execution_start', 'child_start'),
                          ('idle reopen resumed work', frame))
                    if frame.get('id') == 'durable-read':
                        check(frame.get('success') is True, frame)
                        page = frame['data']
                        check(page['limit'] == 64 and not page['hasMore'] and not page['pendingResume'], page)
                        restored = [{k: v for k, v in m.items() if k != 'entryId'} for m in page['messages']]
                        expected = expected_rpc_messages(messages)
                        check(restored == expected and page['total'] == len(messages),
                              ('native reopen canonical history changed/duplicated', restored, expected))
                        check([m['entryId'] for m in page['messages']] ==
                              [r['id'] for r in rows if r.get('kind') == 'entry'
                               and r.get('type') == 'message'],
                              ('native reopen changed entry IDs', page['messages']))
                        item['reopen_messages'] = len(restored)
                        break
            finally:
                try:
                    if reader:
                        reader.close()
                finally:
                    dump(reopen_root / 'rpc-frames.json', reader.frames if reader else [])
            check(len(model.captures) == before_calls and not model.errors,
                  ('idle native read called/rebound provider', before_calls, len(model.captures), model.errors))
            check(root_path.read_bytes() == root_bytes, ('idle child reopen changed root', str(root_path)))
            check(child_path.read_bytes() == child_bytes, ('idle read mutated child', str(child_path)))
            item['reopen_model_calls'] = len(model.captures) - before_calls
            dump(f.root / 'durable-storage.json', evidence)
    return evidence


def run(args, root, version, mode):
    f = Acceptance(root, mode, version)
    model = ChildModel(args, f)
    peer = None
    cleanup_error = None
    try:
        env = environment(args, root, model)
        env.update(TEMP=str(TEMP), TMP=str(TEMP), RPI_PERMISSION_GATE='1')
        # Keep every config/home on C:, including tools that prefer APPDATA.
        for key in ('APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME'):
            p = root / key.lower()
            p.mkdir()
            env[key] = str(p)
        for plugin in (args.ask_plugin, args.gate_plugin):
            check(plugin and plugin.is_file(), ('required real interactive plugin unavailable', plugin))
            shutil.copy2(plugin, root / 'agent' / 'extensions' / plugin.name)
        peer = Client([str(args.bridge)], env, root, f)
        caps = {'subagents': {}, 'fs': {'readTextFile': True, 'writeTextFile': True},
                'terminal': True, 'elicitation': {'form': {}}, '_meta': {OPT_IN: REVISION}}
        if version == 2:
            # v2 removed the v1 fs/terminal flags; opt in explicitly.
            caps.pop('fs')
            caps.pop('terminal')
            caps['_meta']['pi-rust/client-tools'] = {
                'readText': True, 'writeText': True, 'terminal': True}
        init = {'protocolVersion': 1, 'clientCapabilities': caps} if version == 1 else {
            'protocolVersion': 2, 'info': {'name': 'child-tools-native', 'version': '1'}, 'capabilities': caps}
        check(peer.request('initialize', init)['protocolVersion'] == version, 'wrong ACP version')
        for i in range(1 if mode == 'eof' else 2):
            peer.new_root({'cwd': str(root / 'workspace'), 'mcpServers': []}, i)
        peer.retain_native_handles(args.native)
        if os.name == 'nt':
            check(len(peer.handles) == len(f.roots), ('missing actual native root processes', peer.handles))
        for sid, d in f.roots.items():
            exercise = d['mode']
            f.active = sid
            f.boot_roots.discard(sid)
            peer.seq += 1
            rid = peer.seq
            peer.send({'jsonrpc': '2.0', 'id': rid, 'method': 'session/prompt', 'params': {
                'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'ROOT_TOOLS_' + d['marker'] +
                    ': run acceptance. Untrusted model-context JSON: ' + json.dumps({
                        'parentToolCallId': 'FORGED_PARENT_' + d['marker'],
                        '_meta': {'rpi/parentToolCallId': 'FORGED_PARENT_' + d['marker']}})}]}})
            deadline = time.monotonic() + 110
            settled = False
            while True:
                if mode == 'eof' and f.eof_sent and peer.p.poll() is not None and peer.q.empty():
                    break
                try:
                    frame = peer.receive(deadline)
                except AssertionError:
                    if mode == 'eof' and f.eof_sent and f.frames[-1].get('eof'):
                        break
                    raise
                if frame.get('id') == rid and 'method' not in frame:
                    if not (exercise == 'eof' and f.eof_sent):
                        check('result' in frame, ('prompt failed', frame, model.errors))
                        if version == 1:
                            expected = 'cancelled' if exercise == 'cancel' else 'end_turn'
                            check(frame['result'].get('stopReason') == expected, frame)
                            settled = True
                if version == 2 and frame.get('method') == 'session/update':
                    p = frame['params']
                    if p['sessionId'] == sid and p['update']['sessionUpdate'] == 'state_update' and p['update'].get('state') == 'idle':
                        settled = True
                if mode == 'eof':
                    continue
                terminals_finished = all('terminal/release' in t['methods'] for t in f.terminals.values())
                child_terminals = [x['params']['update']['state'] for x in f.frames
                    if x.get('method') == 'session/update' and x['params']['sessionId'] == sid
                    and x['params']['update']['sessionUpdate'] == 'subagent_update'
                    and x['params']['update'].get('state', {}).get('state') == 'idle']
                if settled and terminals_finished and child_terminals:
                    check(len(child_terminals) == 1, ('duplicate child terminal', child_terminals))
                    check(child_terminals[0].get('stopReason') == ('cancelled' if exercise == 'cancel' else 'end_turn'),
                          ('wrong child terminal reason', child_terminals))
                    break
            check(not model.errors, model.errors)
        check(mode != 'eof' or f.eof_sent, 'EOF not triggered')
        result = f.verify()
        # Close every root/native process before scanning any exclusive JSONL.
        peer.close()
        peer = None
        result['durable_storage'] = verify_closed_storage(args, f, env, model)
        result.update(version=version, mode=mode, model_calls=len(model.captures))
        return result
    finally:
        try:
            if peer:
                # Peer.close owns bounded EOF, exact native PID-handle checks,
                # and fixture-tree-only emergency cleanup. stdin may already
                # have been closed by the semantic EOF case (close is idempotent).
                peer.close()
        except Exception:
            cleanup_error = traceback.format_exc()
            raise
        finally:
            model.close()
            dump(root / 'acp-frames.json', f.frames)
            if f.peer:
                with f.peer.conversation_lock:
                    dump(root / 'wire-bidirectional.json', f.peer.conversation)
            dump(root / 'model-inputs.json', model.captures)
            dump(root / 'parent-provenance.json', f.provenance)
            dump(root / 'model-errors.json', model.errors)
            dump(root / 'client-requests.json', f.requests)
            dump(root / 'terminal-lifecycles.json', f.terminals)
            dump(root / 'cleanup-error.json', cleanup_error)


def c_path(path):
    path = path.resolve()
    check(path.drive.lower() == 'c:', ('all inputs/artifacts must be on C:', path))
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('bridge', 'native', 'plugin', 'artifacts'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('ask-plugin', 'gate-plugin'):
        parser.add_argument('--' + name, type=Path)
    parser.add_argument('--wait-native', type=int, default=120)
    parser.add_argument('--cases', nargs='+', choices=['v1-normal', 'v2-normal', 'v1-deny', 'v1-cancel', 'v1-eof'],
                        default=['v1-normal', 'v2-normal', 'v1-deny', 'v1-cancel', 'v1-eof'])
    args = parser.parse_args()
    for name in ('bridge', 'native', 'plugin', 'artifacts', 'ask_plugin', 'gate_plugin'):
        if getattr(args, name):
            setattr(args, name, c_path(getattr(args, name)))
    check(not args.artifacts.exists() or not any(args.artifacts.iterdir()), 'artifacts must be empty')
    args.artifacts.mkdir(parents=True, exist_ok=True)
    TEMP.mkdir(parents=True, exist_ok=True)
    os.environ.update(TEMP=str(TEMP), TMP=str(TEMP))
    report = {'result': 'failed', 'binaries': {}, 'runs': [], 'requested_cases': args.cases,
              'limits': ['No URL elicitation or nested delegation coverage requested.',
                         'A subset --cases run does not prove the full acceptance suite.']}
    try:
        deadline = time.monotonic() + max(0, min(args.wait_native, 600))
        while not args.native.is_file() and time.monotonic() < deadline:
            time.sleep(1)
        for path in (args.bridge, args.native, args.plugin, args.ask_plugin, args.gate_plugin):
            check(path and path.is_file(), ('frozen real binary/plugin unavailable; no substitute', str(path)))
            report['binaries'][str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
        for case in args.cases:
            directory = args.artifacts / case
            directory.mkdir()
            version, mode = case.split('-')
            try:
                result = {'result': 'passed', **run(args, directory, int(version[1:]), mode)}
            except Exception:
                result = {'result': 'failed', 'error': traceback.format_exc()}
            report['runs'].append({'case': case, **result})
            dump(args.artifacts / 'result.json', report)
            print(case + ': ' + result['result'], flush=True)
        report['result'] = 'passed' if all(r['result'] == 'passed' for r in report['runs']) else 'failed'
    except Exception:
        report['precondition_error'] = traceback.format_exc()
        report['runs'] = [{'case': case, 'result': 'failed', 'uncovered': True,
                           'error': report['precondition_error']} for case in args.cases]
    dump(args.artifacts / 'result.json', report)
    print(json.dumps(report, indent=2))
    return int(report['result'] != 'passed')


if __name__ == '__main__':
    sys.dont_write_bytecode = True
    raise SystemExit(main())
