"""ACP child interaction wire checks; all artifacts must be placed on C:."""
import argparse
import json
from pathlib import Path
import queue
import time
from protocol_regression import Bridge


def check(ok, detail):
    if not ok:
        raise AssertionError(detail)


def run(args, version, mode='normal'):
    directory = args.artifacts / f'v{version}-{mode}'
    directory.mkdir(parents=True, exist_ok=True)
    log = directory / 'native.jsonl'
    log.unlink(missing_ok=True)
    b = Bridge(args.bridge, args.rpc, directory / 'sessions', directory, extra_env={
        'RPI_TEST_INTERACTIONS': mode, 'RPI_TEST_INTERACTION_LOG': log})
    frames = []
    try:
        caps = {'subagents': {}, 'fs': {'readTextFile': True}, 'terminal': True,
                'elicitation': {'form': {}, 'url': {}},
                '_meta': {'pi-rust/client-tools': {'readText': True, 'terminal': True}}}
        if mode == 'unnegotiated':
            caps.pop('subagents')
        init = {'protocolVersion': version, 'clientCapabilities': caps} if version == 1 else {
            'protocolVersion': 2, 'info': {'name': 'child-tests', 'version': '1'}, 'capabilities': caps}
        reply, _ = b.request('initialize', init)
        check('result' in reply, reply)
        roots = [b.request('session/new', {'cwd': str(directory), 'mcpServers': []})[0]['result']['sessionId'] for _ in range(2)]
        for root in roots if mode == 'normal' else roots[:1]:
            child = f'child:{len(root)}:{root}:worker'
            b.next_id += 1
            prompt_id = b.next_id
            b.send({'jsonrpc': '2.0', 'id': prompt_id, 'method': 'session/prompt',
                    'params': {'sessionId': root, 'prompt': [{'type': 'text', 'text': 'interact'}]}})
            announced = False
            requests = []
            done = False
            cancelled = False
            cleanup = set()
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                m = b.output.get(timeout=max(.01, deadline - time.monotonic()))
                frames.append(m)
                p = m.get('params', {})
                update = p.get('update', {})
                if update.get('sessionUpdate') == 'subagent_update' and update.get('sessionId') == child:
                    check(p['sessionId'] == root, m)
                    check(update.get('capabilities', {}) == {}, m)
                    announced = True
                method = m.get('method', '')
                if method in ('fs/read_text_file', 'terminal/create', 'session/request_permission', 'elicitation/create', 'terminal/kill', 'terminal/release'):
                    check(mode not in ('forged', 'unannounced', 'unnegotiated'), m)
                    is_root = method == 'fs/read_text_file' and p.get('sessionId') == root
                    check(is_root or (announced and p.get('sessionId') == child), m)
                    requests.append(m)
                    if mode in ('cancel', 'disconnect') and len(requests) == 6:
                        if mode == 'disconnect':
                            b.close()
                            return
                        b.send({'jsonrpc': '2.0', 'method': 'session/cancel', 'params': {'sessionId': root}})
                        cancelled = True
                    if mode in ('cancel', 'disconnect') and method not in ('terminal/kill', 'terminal/release'):
                        continue
                    if method == 'fs/read_text_file': result = {'content': 'fixture'}
                    elif method == 'terminal/create': result = {'terminalId': 'term'}
                    elif method == 'session/request_permission': result = {'outcome': {'outcome': 'selected', 'optionId': '0'}}
                    elif method == 'elicitation/create': result = {'action': 'accept', 'content': {'answer': 'Rust'}} if p['mode'] == 'form' else {'action': 'accept'}
                    else:
                        check(p['sessionId'] == child and cancelled, m)
                        cleanup.add(method)
                        result = {}
                    b.send({'jsonrpc': '2.0', 'id': m['id'], 'result': result})
                if mode in ('forged', 'unannounced', 'unnegotiated'):
                    if m.get('id') == prompt_id and 'error' in m:
                        done = True
                        break
                    if update.get('stopReason') == 'error':
                        done = True
                        break
                elif version == 1 and m.get('id') == prompt_id and 'method' not in m:
                    check(m.get('result', {}).get('stopReason') == ('cancelled' if mode == 'cancel' else 'end_turn'), m)
                    done = True
                    break
                elif version == 2 and p.get('sessionId') == root and update.get('sessionUpdate') == 'state_update' and update.get('state') == 'idle':
                    done = True
                    break
            check(done, frames)
            if mode in ('forged', 'unannounced', 'unnegotiated'):
                check(not requests, requests)
                check(any('interaction routing failed' in e for e in b.errors), b.errors)
                # Terminating the malformed source must not kill another root.
                reply, _ = b.request('session/set_config_option', {'sessionId': roots[1], 'configId': 'unknown', 'value': 'x'})
                check('error' in reply, reply)
            elif mode == 'cancel':
                check(cleanup == {'terminal/kill', 'terminal/release'}, cleanup)
                # Responses to cancelled client requests must not reach native.
                for m in requests[:6]:
                    b.send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'content': 'late'}})
            else:
                check(len(requests) == 6, requests)
                reply, _ = b.request('session/prompt', {'sessionId': child, 'prompt': []})
                check(reply.get('error', {}).get('code') == -32602, reply)
        if mode == 'normal':
            messages = [json.loads(line) for line in log.read_text().splitlines()]
            for root in roots:
                replies = [x['message'] for x in messages if x['root'] == root and x['message']['type'] in ('client_response', 'ui_dialog_response')]
                check({x['requestId'] for x in replies} == {'child-fs', 'root-fs', 'child-terminal', 'child-permission', 'child-form', 'child-url'}, replies)
                check(all(('result' in x or 'answer' in x) if x['requestId'] != 'child-url'
                          else x.get('urlElicitation') == {'action': 'accept'} and not x.get('cancelled')
                          for x in replies), replies)
    finally:
        b.close()
        (directory / 'frames.json').write_text(json.dumps(frames, indent=2))
        (directory / 'stderr.log').write_text(''.join(b.errors))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--rpc', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    for version, mode in [(1, 'normal'), (2, 'normal'), (1, 'forged'), (1, 'unannounced'),
                          (1, 'unnegotiated'), (1, 'cancel'), (1, 'disconnect')]:
        run(args, version, mode)
        print(f'PASS v{version} {mode}', flush=True)
