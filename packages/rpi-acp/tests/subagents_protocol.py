"""Live child-session wire checks against the actual ACP bridge and fake_rpc.
All runtime artifacts go under --artifacts (on C: in the review environment).
"""
import argparse
import json
from pathlib import Path
import queue
import time
from protocol_regression import Bridge

OPT_IN = 'rpi/unstable-session-rfds'
REVISION = '1142c83bbfa9407f9f264d1e290ffafcd54eb79e'


def check(value, detail):
    if not value:
        raise AssertionError(detail)


def run(args, version, opt, mode='normal'):
    name = f'v{version}-{opt}-{mode}'
    root = args.artifacts / name
    root.mkdir(parents=True)
    env_log = root / 'native-launch.jsonl'
    frames = []
    bridge = Bridge(args.bridge, args.rpc, root / 'sessions', root, extra_env={
        'RPI_TEST_CHILD_MODE': mode, 'RPI_TEST_CHILD_LOG': env_log,
        # Poison the inherited environment: unnegotiated roots MUST remove it.
        'RPI_CHILD_EVENTS': 'inherited-poison',
    })

    def request(method, params, success=True):
        reply, messages = bridge.request(method, params)
        frames.extend(messages + [reply])
        check(('result' in reply) == success, reply)
        return reply, messages

    def prompt(sid):
        _, messages = request('session/prompt', {'sessionId': sid,
            'prompt': [{'type': 'text', 'text': 'delegate'}]})
        deadline = time.monotonic() + 10
        while True:
            child_idle = any(m.get('params', {}).get('update', {}).get('state', {}).get('state') == 'idle'
                for m in messages if isinstance(m.get('params', {}).get('update', {}).get('state'), dict))
            root_idle = any(m.get('params', {}).get('sessionId') == sid and
                m.get('params', {}).get('update', {}).get('sessionUpdate') == 'state_update' and
                m['params']['update'].get('state') == 'idle' for m in messages)
            if (not enabled or child_idle) and (version == 1 or root_idle):
                return messages
            message = bridge.output.get(timeout=max(.01, deadline - time.monotonic()))
            frames.append(message)
            messages.append(message)

    enabled = version == 2 or opt is True
    try:
        caps = {'_meta': {OPT_IN: REVISION}}
        if opt is not None:
            caps['subagents'] = opt if opt is not True else {}
        params = {'protocolVersion': version, 'clientCapabilities': caps} if version == 1 else {
            'protocolVersion': 2, 'info': {'name': 'child-wire', 'version': '1'}, 'capabilities': caps}
        reply, _ = request('initialize', params)
        check('subagents' not in reply['result'].get('agentCapabilities', {}), reply)
        sid = request('session/new', {'cwd': str(root), 'mcpServers': []})[0]['result']['sessionId']
        if mode != 'normal':
            bridge.next_id += 1
            rid = bridge.next_id
            bridge.send({'jsonrpc': '2.0', 'id': rid, 'method': 'session/prompt',
                'params': {'sessionId': sid, 'prompt': [{'type': 'text', 'text': 'invalid child'}]}})
            # v2 may acknowledge insertion before the failed event stream. The
            # explicit diagnostic and failed prompt/idle prove failure, not loss.
            deadline = time.monotonic() + 12
            failure = False
            while time.monotonic() < deadline:
                try:
                    message = bridge.output.get(timeout=.2)
                    frames.append(message)
                    if message.get('id') == rid and 'error' in message:
                        failure = True
                    update = message.get('params', {}).get('update', {})
                    if update.get('sessionUpdate') == 'state_update' and update.get('stopReason') == 'error':
                        failure = True
                except queue.Empty:
                    pass
                if failure and any('subagent forwarding failed:' in e for e in bridge.errors):
                    break
            check(failure, frames)
            check(any('subagent forwarding failed:' in e for e in bridge.errors), bridge.errors)
            expected = {'unknown': 'unsupported child event type', 'malformed': 'malformed',
                        'oversize': '256 KiB', 'missing_id': 'canonical'}[mode]
            check(any(expected in e for e in bridge.errors), bridge.errors)
            return

        roots = [sid, request('session/new', {'cwd': str(root), 'mcpServers': []})[0]['result']['sessionId']]
        child_ids = []
        for sid in roots:
            messages = prompt(sid)
            announcements = [m for m in messages if m.get('params', {}).get('update', {}).get('sessionUpdate') == 'subagent_update']
            if not enabled:
                check(not announcements, messages)
                check(not any(str(m.get('params', {}).get('sessionId', '')).startswith('child:') for m in messages), messages)
                continue
            check(announcements, messages)
            cid = announcements[0]['params']['update']['sessionId']
            child_ids.append(cid)
            check(cid == f'child:{len(sid)}:{sid}:child', cid)
            known = {sid}
            text = ''
            for m in messages:
                if m.get('method') != 'session/update':
                    continue
                p = m['params']
                check(p['sessionId'] in known, ('traffic before announcement', m))
                u = p['update']
                if u['sessionUpdate'] == 'subagent_update':
                    check(p['sessionId'] == sid, m)
                    check(not u.get('capabilities'), m)
                    known.add(u['sessionId'])
                if p['sessionId'] == cid and u['sessionUpdate'] == 'agent_message_chunk':
                    text += u['content']['text']
                    if version == 2:
                        check(u['messageId'] == 'child-message', m)
            check(text == 'child hello world', messages)
            for method in ['session/prompt', 'session/load', 'session/resume', 'session/fork',
                           'session/close', 'session/set_mode', 'session/set_config_option']:
                reply, _ = request(method, {'sessionId': cid}, success=False)
                check(reply['error']['code'] == -32602, reply)
        if enabled:
            check(len(set(child_ids)) == 2, child_ids)
        # Fork staging is intentionally unexposed; the reopened fork is a root
        # and must inherit negotiation exactly like fresh and reloaded roots.
        fork, _ = request('session/fork', {'sessionId': roots[0], 'cwd': str(root), 'mcpServers': []})
        fid = fork['result']['sessionId']
        prompt(fid)
        request('session/close', {'sessionId': roots[0]})
        request('session/resume', {'sessionId': roots[0], 'cwd': str(root), 'mcpServers': []})
        prompt(roots[0])
        launches = [json.loads(line) for line in env_log.read_text().splitlines()]
        check(any(x['fork'] for x in launches), launches)
        check(any(x['reload'] and x['sessionId'] == fid for x in launches), launches)
        check(sum(x['sessionId'] == roots[0] for x in launches) == 2, launches)
        for launch in launches:
            check(launch['childEvents'] == ('1' if enabled and not launch['fork'] else None), launch)
    finally:
        bridge.close()
        (root / 'frames.json').write_text(json.dumps(frames, ensure_ascii=False, indent=2), encoding='utf-8')
        (root / 'stderr.log').write_text(''.join(bridge.errors), encoding='utf-8')
    print('PASS', name, flush=True)


def main():
    parser = argparse.ArgumentParser()
    for name in ['bridge', 'rpc', 'artifacts']:
        parser.add_argument('--' + name, required=True, type=Path)
    args = parser.parse_args()
    for version, opt in [(1, True), (1, None), (1, False), (2, None)]:
        run(args, version, opt)
    for version in [1, 2]:
        # Canonical message IDs are required by the v2 chunk contract; v1
        # explicitly allows identity-less chunks, so absence is not malformed.
        modes = ['unknown', 'malformed', 'oversize']
        if version == 2:
            modes.append('missing_id')
        for mode in modes:
            run(args, version, True, mode)
            print('PASS', f'v{version}-{mode}', flush=True)


if __name__ == '__main__':
    main()
