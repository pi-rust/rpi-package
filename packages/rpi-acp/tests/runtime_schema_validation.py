"""Acceptance-only validation of actual ACP captures; never builds or runs native code.

Use the existing jsonschema environment (no installation needed):
  python runtime_schema_validation.py --schema-dir PINNED_DIR --captures SUITE [SUITE ...]
Directories discover acp-frames.json / frames.json, not model traffic or transcripts.
Bare arrays from current suites are agent->client only. No request is invented to
bind their replies. For full correlation supply an ordered JSON array of records:
  {"direction": "client_to_agent" | "agent_to_client", "frame": {...}}
Both schemas must match the pinned full-file digests. Output is JSON on stdout;
exit 1 means a validation failure, exit 2 means incomplete evidence, 0 means all
provided frames were bound/validated (NOT all ACP messages/RFDs implemented).
--self-test exercises validator controls separately, never runtime coverage.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import sys

from jsonschema import Draft202012Validator
from jsonschema.exceptions import ValidationError

DIGESTS = {
    1: '1c57095acf9c0844054dd1fdc3624c121b3fdc24209ecc987fd6748c705fdef5',
    2: '932d41d2ba07142af6e7294675337154a034e2ce033c096fc079053a76b0925e',
}
# Protocol method bindings, not a permissive envelope/result union. Each entry
# is verified to be a member of the corresponding official envelope below.
CLIENT = {
    'initialize': 'Initialize', 'authenticate': 'Authenticate', 'logout': 'Logout',
    'auth/login': 'LoginAuth', 'auth/logout': 'LogoutAuth',
    'session/new': 'NewSession', 'session/load': 'LoadSession',
    'session/list': 'ListSessions', 'session/delete': 'DeleteSession',
    'session/resume': 'ResumeSession', 'session/close': 'CloseSession',
    'session/set_mode': 'SetSessionMode',
    'session/set_config_option': 'SetSessionConfigOption', 'session/prompt': 'Prompt',
}
AGENT = {
    'fs/read_text_file': 'ReadTextFile', 'fs/write_text_file': 'WriteTextFile',
    'terminal/create': 'CreateTerminal', 'terminal/output': 'TerminalOutput',
    'terminal/release': 'ReleaseTerminal', 'terminal/kill': 'KillTerminal',
    'terminal/wait_for_exit': 'WaitForTerminalExit',
    'session/request_permission': 'RequestPermission', 'elicitation/create': 'CreateElicitation',
}


def pointer(parts):
    return ''.join('/' + str(p).replace('~', '~0').replace('/', '~1') for p in parts)


def refs(node):
    if isinstance(node, dict):
        if '$ref' in node:
            yield node['$ref'].split('/')[-1]
        for value in node.values():
            yield from refs(value)
    elif isinstance(node, list):
        for value in node:
            yield from refs(value)


class Unsupported(Exception):
    pass


class Official:
    def __init__(self, directory, version):
        self.version = version
        self.path = directory / f'schema-v{version}.json'
        raw = self.path.read_bytes()
        digest = hashlib.sha256(raw).hexdigest()
        if digest != DIGESTS[version]:
            raise ValueError(f'{self.path}: digest {digest}, expected {DIGESTS[version]}')
        self.schema = json.loads(raw)
        Draft202012Validator.check_schema(self.schema)
        self.defs = self.schema['$defs']

    def validator(self, node):
        return Draft202012Validator({'$defs': self.defs, 'allOf': [node]})

    def resolve(self, node):
        return self.defs[node['$ref'].split('/')[-1]] if '$ref' in node else node

    def constants(self, node):
        node = self.resolve(node)
        result = {k: v['const'] for k, v in node.get('properties', {}).items() if 'const' in v}
        for child in node.get('allOf', []):
            result.update(self.constants(child))
        return result

    def selected(self, node, value, path=()):
        """Additional checks select known variants; original schema is unmodified.

        Never use a future/catch-all branch as proof of a known variant. Walk
        nested objects too (content, diff, auth, permission, plan/state unions).
        """
        node = self.resolve(node)
        for keyword in ('oneOf', 'anyOf'):
            branches = node.get(keyword, [])
            matches = [b for b in branches if self.constants(b) and isinstance(value, dict)
                       and all(value.get(k) == v for k, v in self.constants(b).items())]
            if matches:
                for branch in matches:
                    for error in self.validator(branch).iter_errors(value):
                        yield path + tuple(error.absolute_path), error
                    yield from self.selected(branch, value, path)
                return
            if branches and isinstance(value, dict):
                common = set.intersection(*(set(self.resolve(b).get('required', [])) for b in branches))
                for key in sorted(common - value.keys()):
                    yield path, ValidationError('required property is missing', validator='required',
                                                validator_value=[key], instance=value)
            # Undiscriminated unions: traverse only schema-valid alternatives.
            for branch in branches:
                if self.validator(branch).is_valid(value):
                    yield from self.selected(branch, value, path)
            if branches:
                return
        for child in node.get('allOf', []):
            yield from self.selected(child, value, path)
        if isinstance(value, dict):
            for key, child in node.get('properties', {}).items():
                if key in value:
                    yield from self.selected(child, value[key], path + (key,))
        if isinstance(value, list) and isinstance(node.get('items'), dict):
            for i, item in enumerate(value):
                yield from self.selected(node['items'], item, path + (i,))

    def errors(self, name, value):
        if name not in self.defs:
            raise Unsupported(f'absent official definition {name}')
        node = {'$ref': f'#/$defs/{name}'}
        errors = [(tuple(e.absolute_path), e) for e in self.validator(node).iter_errors(value)]
        errors.extend(self.selected(node, value))
        return errors

    def binding(self, method, sender, suffix):
        stem = (CLIENT if sender == 'client_to_agent' else AGENT).get(method)
        if stem is None:
            raise Unsupported(f'no official method binding for {method}')
        name = stem + suffix
        envelope = ('ClientRequest' if sender == 'client_to_agent' else 'AgentRequest') if suffix == 'Request' else (
            'AgentResponse' if sender == 'client_to_agent' else 'ClientResponse')
        if name not in set(refs(self.defs[envelope])):
            raise Unsupported(f'{name} not in official v{self.version} {envelope}')
        return name

    def update_variant(self, update):
        tag = update.get('sessionUpdate') if isinstance(update, dict) else None
        node = self.defs['SessionUpdate']
        variants = node.get('oneOf', node.get('anyOf', []))
        for branch in variants:
            if self.constants(branch).get('sessionUpdate') == tag and tag is not None:
                return branch
        raise Unsupported(f'SessionUpdate/{tag}: no known official variant (future fallback is not acceptance)')


def load(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def capture_files(paths):
    files, missing = set(), []
    for path in paths:
        if path.is_dir():
            found = {p for name in ('acp-frames.json', 'frames.json') for p in path.rglob(name)}
            if not found:
                missing.append(str(path.resolve()))
            files.update(found)
        elif path.is_file():
            files.add(path)
        else:
            missing.append(str(path.resolve()))
    return sorted(files), missing


def validate_capture(path, schemas):
    records = load(path)
    if not isinstance(records, list):
        raise ValueError(f'{path}: expected ordered frame array')
    frames = [(r['direction'], r['frame']) if 'direction' in r and 'frame' in r
              else ('agent_to_client', r) for r in records]
    versions = {f['result']['protocolVersion'] for _, f in frames
                if isinstance(f.get('result'), dict) and 'protocolVersion' in f['result']}
    versions.update(f['params']['protocolVersion'] for _, f in frames
                    if f.get('method') == 'initialize')
    negotiation_sources = []
    for metadata_name in ('initialize.json', 'protocol-version.json'):
        metadata_path = path.parent / metadata_name
        if metadata_path.is_file():
            metadata = load(metadata_path)
            negotiated = metadata.get('result', metadata) if isinstance(metadata, dict) else metadata
            negotiated = negotiated.get('protocolVersion') if isinstance(negotiated, dict) else negotiated
            if type(negotiated) is int:
                versions.add(negotiated)
                negotiation_sources.append(str(metadata_path.resolve()))
    if len(versions) != 1 or next(iter(versions)) not in schemas:
        raise ValueError(f'{path}: missing/ambiguous protocol negotiation: {versions}')
    version = next(iter(versions))
    official = schemas[version]
    report = {'path': str(path.resolve()), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
              'version': version, 'negotiation_metadata': negotiation_sources,
              'frames': len(frames), 'validated': 0, 'failures': [],
              'unsupported': [], 'unbound_replies': [], 'pending_requests': [],
              'coverage': Counter(), 'errors': Counter()}
    pending = {}

    def issue(index, method, definition, at, message, schema_pointer=None):
        report['failures'].append(dict(frame=index, method=method, definition=definition,
                                       pointer=at, message=message, schema_pointer=schema_pointer))

    def check(index, method, definition, value, prefix):
        for parts, error in official.errors(definition, value):
            # Do not present failures from unrelated union alternatives as runtime
            # defects. selected() separately checks the actual discriminant.
            at = prefix + pointer(parts)
            if error.validator == 'required' and isinstance(error.instance, dict):
                missing = [key for key in error.validator_value if key not in error.instance]
                for key in missing:
                    issue(index, method, definition, at + pointer((key,)),
                          'required property is missing', pointer(error.absolute_schema_path))
            else:
                issue(index, method, definition, at, error.message,
                      pointer(error.absolute_schema_path))

    report['capture_markers'] = []
    report['payload_subset'] = Counter()
    for i, (direction, frame) in enumerate(frames):
        # This exact fixture-owned EOF marker is not a transmitted JSON-RPC frame.
        if frame == {'eof': True}:
            report['capture_markers'].append({'frame': i, 'marker': 'eof'})
            continue
        method = frame.get('method')
        before = len(report['failures'])
        try:
            if direction not in ('client_to_agent', 'agent_to_client'):
                raise ValueError(f'bad capture direction {direction}')
            if frame.get('jsonrpc') != '2.0':
                issue(i, method, 'JSON-RPC', '/jsonrpc', 'expected 2.0')
            if method is not None:
                if not isinstance(method, str) or 'result' in frame or 'error' in frame:
                    issue(i, method, 'JSON-RPC', '', 'invalid request/notification envelope')
                    continue
                if 'id' in frame:
                    check(i, method, 'RequestId', frame['id'], '/id')
                    key = (direction, type(frame['id']).__name__, frame['id'])
                    if key in pending:
                        issue(i, method, 'RequestId', '/id', 'duplicate outstanding request ID')
                    pending[key] = method
                    definition = official.binding(method, direction, 'Request')
                    check(i, method, definition, frame.get('params'), '/params')
                elif method == 'session/update':
                    params = frame.get('params', {})
                    check(i, method, 'SessionId', params.get('sessionId'), '/params/sessionId')
                    update = params.get('update')
                    for obj, prefix in ((params, '/params'), (update, '/params/update')):
                        if isinstance(obj, dict):
                            for k in obj:
                                if 'parenttoolcallid' in k.lower():
                                    issue(i, method, 'ordinary _meta extension', prefix + '/' + k,
                                          'parent association must be in _meta, not a protocol field')
                    branch = official.update_variant(update)
                    tag = update['sessionUpdate']
                    for content in update.get('content', []) if isinstance(update.get('content'), list) else []:
                        if content.get('type') == 'diff':
                            report['payload_subset']['diff'] += 1
                    if tag in ('plan', 'plan_update'):
                        report['payload_subset']['plan'] += 1
                    if tag in ('agent_message_chunk', 'agent_message'):
                        report['payload_subset']['model_output'] += 1
                    if tag in ('tool_call', 'tool_call_update'):
                        report['payload_subset']['tool'] += 1
                    report['coverage'][tag] += 1
                    # Validate the entire official notification AND the known branch.
                    check(i, method, 'SessionNotification' if version == 1 else 'UpdateSessionNotification', params, '/params')
                    for e in official.validator(branch).iter_errors(update):
                        issue(i, method, f'SessionUpdate/{tag}', '/params/update' + pointer(e.absolute_path), e.message,
                              pointer(e.absolute_schema_path))
                else:
                    notifications = {'session/cancel': 'CancelNotification' if version == 1 else 'CancelSessionNotification',
                                     '$/cancel_request': 'CancelRequestNotification',
                                     'elicitation/complete': 'CompleteElicitationNotification'}
                    definition = notifications.get(method)
                    if definition is None:
                        raise Unsupported(f'no official notification binding for {method}')
                    check(i, method, definition, frame.get('params'), '/params')
            else:
                if 'id' not in frame or ('result' in frame) == ('error' in frame):
                    issue(i, None, 'JSON-RPC', '', 'reply requires id and exactly one result/error')
                    continue
                check(i, None, 'RequestId', frame['id'], '/id')
                sender = 'client_to_agent' if direction == 'agent_to_client' else 'agent_to_client'
                key = (sender, type(frame['id']).__name__, frame['id'])
                method = pending.pop(key, None)
                if method is None:
                    report['unbound_replies'].append({'frame': i, 'id': frame['id'],
                                                      'reason': 'originating request not captured; no method inferred from result shape/ID'})
                if 'error' in frame:
                    check(i, method, 'Error', frame['error'], '/error')
                    code = frame['error'].get('code')
                    report['errors'][str(code)] += 1
                    if type(code) is not int or not -(2**31) <= code < 2**31:
                        issue(i, method, 'ErrorCode', '/error/code', 'expected signed int32')
                    elif code not in (-32700, -32600, -32601, -32602, -32603, -32800) and not -32099 <= code <= -32000:
                        issue(i, method, 'ErrorCode category', '/error/code', 'not a standard/cancel/server-error category')
                elif method is not None:
                    definition = official.binding(method, sender, 'Response')
                    check(i, method, definition, frame['result'], '/result')
                else:
                    continue
            if len(report['failures']) == before:
                report['validated'] += 1
        except Unsupported as error:
            report['unsupported'].append({'frame': i, 'method': method, 'reason': str(error)})
    report['pending_requests'] = [{'direction': d, 'id': ident, 'method': method}
                                  for (d, _, ident), method in pending.items()]
    # Pending requests can be legitimate on EOF/cancellation; not schema failures,
    # but cannot be called round-trip acceptance without captured responses.
    report['failed_frames'] = len({f['frame'] for f in report['failures']})
    report['status'] = ('FAIL' if report['failures'] else 'INCOMPLETE'
                        if report['unsupported'] or report['unbound_replies'] or report['pending_requests']
                        else 'PASS_PROVIDED_FRAMES_ONLY')
    return report


def self_test(schemas):
    controls = 0
    for version, official in schemas.items():
        bad = [('PromptRequest', {'sessionId': 's', 'prompt': [{'type': 'text'}]}),
               ('ToolCallUpdate', {'toolCallId': 7}),
               ('CreateElicitationRequest', {'mode': 'form', 'sessionId': 's'}),
               ('ContentBlock', {'type': 'image', 'data': 7}),
               ('ToolCallContent', {'type': 'diff', 'path': 7}),
               ('AuthMethod', {'type': 'terminal'}),
               ('RequestPermissionOutcome', {'outcome': 'selected'}),
               ('PlanEntry', {'content': 'x', 'priority': 7, 'status': 'pending'}),
               ('Error', {'code': 'bad', 'message': 'x'})]
        for name, value in bad:
            assert official.errors(name, value), (version, name)
            controls += 1
        good = {'toolCallId': 't', '_meta': {'rpi/parentToolCallId': 'p'}}
        assert not official.errors('ToolCallUpdate', good)
        controls += 1
        try:
            official.update_variant({'sessionUpdate': 'subagent_update'})
        except Unsupported:
            controls += 1
        else:
            raise AssertionError('draft variant incorrectly accepted')
        branch = official.update_variant({'sessionUpdate': 'tool_call_update'})
        assert list(official.validator(branch).iter_errors({'sessionUpdate': 'tool_call_update'}))
        controls += 1
    # A future fallback remains official-schema valid, but not known acceptance.
    assert not schemas[2].errors('SessionUpdate', {'sessionUpdate': '_future'})
    try:
        schemas[2].update_variant({'sessionUpdate': '_future'})
    except Unsupported:
        controls += 1
    else:
        raise AssertionError('future fallback treated as known variant')
    for bad in (2**31, -(2**31)-1, 'bad'):
        # JSON Schema format is annotation-only: explicit bound check is required.
        assert type(bad) is not int or not -(2**31) <= bad < 2**31
        controls += 1
    return controls


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--schema-dir', required=True, type=Path)
    parser.add_argument('--captures', nargs='+', type=Path, default=[])
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    schemas = {v: Official(args.schema_dir, v) for v in (1, 2)}
    files, missing = capture_files(args.captures)
    result = {'schema_digests': DIGESTS, 'missing_captures': missing, 'captures': [],
              'control_checks': self_test(schemas) if args.self_test else 0}
    for path in files:
        result['captures'].append(validate_capture(path, schemas))
    failed = any(c['failures'] for c in result['captures'])
    incomplete = bool(missing) or not files or any(c['unsupported'] or c['unbound_replies'] or c['pending_requests']
                                                 for c in result['captures'])
    result['per_version'] = {}
    for version in schemas:
        captures = [c for c in result['captures'] if c['version'] == version]
        totals = {key: sum(c[key] for c in captures) for key in ('frames', 'validated', 'failed_frames')}
        totals.update({key: sum(len(c[key]) for c in captures)
                       for key in ('unsupported', 'unbound_replies', 'pending_requests')})
        totals['captures'] = len(captures)
        totals['status'] = ('FAIL' if totals['failed_frames'] else 'INCOMPLETE'
                            if not captures or any(c['status'] == 'INCOMPLETE' for c in captures)
                            else 'PASS_PROVIDED_FRAMES_ONLY')
        result['per_version'][version] = totals
    result['status'] = 'FAIL' if failed else 'INCOMPLETE' if incomplete else 'PASS_PROVIDED_FRAMES_ONLY'
    print(json.dumps(result, indent=2, ensure_ascii=True))
    return 1 if failed else 2 if incomplete else 0


if __name__ == '__main__':
    sys.exit(main())
