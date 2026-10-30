"""Real ACP bridge + fake native, MCP2026 inputs; stdlib only, no model/network."""
import argparse
import json
from pathlib import Path
import tempfile
import time
from protocol_regression import Bridge


def run(bridge_path, rpc):
    for version in (1, 2):
        with tempfile.TemporaryDirectory(prefix='acp-client-mcp-') as tmp:
            root = Path(tmp)
            extra = root / 'extra'; extra.mkdir()
            log = root / 'native.jsonl'
            bridge = Bridge(bridge_path, rpc, root / 'sessions', root,
                            extra_env={'RPI_TEST_MCP_NATIVE_LOG': log})
            seen = []; elicitations = []; ids = set(); attempts = {}
            pending = None; pending_call = None; cancelled = False
            sid = None

            def reply(frame, value):
                bridge.send({'jsonrpc':'2.0','id':frame['id'],'result':value})

            def handle(frame):
                nonlocal pending, pending_call, cancelled
                method = frame.get('method')
                if method == 'mcp/message':
                    op = frame['params']; seen.append(op)
                    assert op['serverId'] == 'provider:stable', op
                    assert op['requestId'] not in ids, op
                    ids.add(op['requestId'])
                    params = op['params']
                    metadata = params['_meta']
                    assert metadata['io.modelcontextprotocol/protocolVersion'] == '2026-07-28'
                    caps = metadata['io.modelcontextprotocol/clientCapabilities']
                    assert caps.get('elicitation') == {'form':{}, 'url':{}}, caps
                    assert caps.get('roots') == {} and 'sampling' not in caps, caps
                    if op['method'] == 'tools/list':
                        value = {'tools':[{'name':'echo','inputSchema':{'type':'object'}}]}
                    else:
                        assert op['method'] == 'tools/call' and params['name'] == 'echo', op
                        mode = params['arguments']['mode']
                        n = attempts.get(mode, 0); attempts[mode] = n + 1
                        if mode == 'multiround':
                            if n == 0:
                                assert 'inputResponses' not in params and 'requestState' not in params
                                value = {'resultType':'input_required','requestState':'opaque:do-not-parse',
                                         'inputRequests':{'form-key':{'method':'elicitation/create','params':{
                                             'mode':'form','message':'Pick a count','requestedSchema':{
                                                 'type':'object','properties':{'count':{'type':'integer','minimum':1}},
                                                 'required':['count'],'additionalProperties':False}}},
                                                          'roots-key':{'method':'roots/list'}}}
                            elif n == 1:
                                assert params['requestState'] == 'opaque:do-not-parse'
                                assert params['inputResponses']['form-key'] == {'action':'accept','content':{'count':2},'future':True}
                                roots = params['inputResponses']['roots-key']['roots']
                                assert {r['uri'].rstrip('/') for r in roots} == {root.as_uri(), extra.as_uri()}, roots
                                value = {'resultType':'input_required','inputRequests':{'url-key':{
                                    'method':'elicitation/create','params':{'mode':'url','message':'Sign in',
                                        'url':'https://example.com/authorize','elicitationId':'server-url-id'}}}}
                            elif n == 2:
                                assert 'requestState' not in params, params
                                assert params['inputResponses'] == {'url-key':{'action':'decline','future':'verbatim'}}, params
                                value = {'resultType':'input_required','requestState':'last-opaque',
                                         'inputRequests':{'cancel-key':{'method':'elicitation/create','params':{
                                             'message':'Last decision','requestedSchema':{'type':'object','properties':{}}}}}}
                            else:
                                assert n == 3 and params['requestState'] == 'last-opaque'
                                assert params['inputResponses'] == {'cancel-key':{'action':'cancel'}}, params
                                value = {'resultType':'complete','content':[],'future':{'opaque':True}}
                        elif mode in ('pending', 'invalid', 'state-only'):
                            assert n == 0, 'unsafe input must not retry'
                            pending_call = op['requestId']
                            value = {'resultType':'input_required','requestState':'opaque'}
                            if mode != 'state-only':
                                value['inputRequests'] = {'decision':{'method':'elicitation/create','params':{
                                    'mode':'form','message':mode,'requestedSchema':{'type':'object',
                                        'properties':{'count':{'type':'integer'}},'required':['count']}}}}
                        else:
                            value = {'resultType':'complete','content':[],'future':{'opaque':True}}
                    reply(frame, {'result':value, '_meta':None})
                elif method == 'elicitation/create':
                    params = frame['params']; elicitations.append(params)
                    assert params['sessionId'] == sid, params
                    assert 'requestState' not in params
                    if params['message'] == 'pending':
                        assert pending is None
                        pending = frame['id']
                    elif params['message'] == 'invalid':
                        reply(frame, {'action':'accept','content':{'count':'wrong-type'}})
                    elif params.get('mode') == 'url':
                        assert params['url'] == 'https://example.com/authorize'
                        assert params['elicitationId'] == 'server-url-id'
                        reply(frame, {'action':'decline','future':'verbatim'})
                    elif params['message'] == 'Last decision':
                        reply(frame, {'action':'cancel'})
                    else:
                        assert params['requestedSchema']['properties']['count']['type'] == 'integer'
                        reply(frame, {'action':'accept','content':{'count':2},'future':True})
                elif method == '$/cancel_request':
                    assert frame['params'] == {'requestId':pending}, frame
                    cancelled = True

            def request(method, params):
                bridge.next_id += 1; request_id = bridge.next_id
                bridge.send({'jsonrpc':'2.0','id':request_id,'method':method,'params':params})
                while True:
                    frame = bridge.output.get(timeout=15)
                    handle(frame)
                    if frame.get('id') == request_id and 'method' not in frame:
                        assert 'result' in frame, frame
                        return frame['result']

            def prompt(session, mode, wait=True):
                result = request('session/prompt', {'sessionId':session,'prompt':[{'type':'text','text':mode}]})
                if version == 2 and wait:
                    while True:
                        frame = bridge.output.get(timeout=15); handle(frame)
                        if frame.get('params',{}).get('sessionId') == session and frame.get('params',{}).get('update',{}).get('state') == 'idle':
                            break
                return result

            def records():
                return [json.loads(line) for line in log.read_text().splitlines()]

            try:
                meta = {'_meta':{'rpi/unstable-session-rfds':'1142c83bbfa9407f9f264d1e290ffafcd54eb79e'},
                        'elicitation':{'form':{},'url':{}}}
                init = {'protocolVersion':version}
                init.update({'info':{'name':'test','version':'1'},'capabilities':meta} if version == 2 else {'clientCapabilities':meta})
                caps = request('initialize', init)
                assert (caps['capabilities']['session']['mcp']['acp'] == {} if version == 2 else caps['agentCapabilities']['mcpCapabilities']['acp'] is True)
                declaration = {'type':'acp','name':'Client tools / human','serverId':'provider:stable'}
                config = {'cwd':str(root),'mcpServers':[declaration], 'additionalDirectories':[str(extra)]}
                sid = request('session/new', config)['sessionId']
                sibling = request('session/new', config)['sessionId']
                prompt(sid, 'multiround')
                assert attempts['multiround'] == 4
                responses = [r for r in records() if r.get('type') == 'external_tool_response']
                assert responses[-1]['result']['future'] == {'opaque':True}, responses[-1]
                for mode in ('invalid', 'state-only'):
                    prompt(sid, mode)
                    response = [r for r in records() if r.get('type') == 'external_tool_response'][-1]
                    assert response.get('isError') or response.get('error'), response
                    assert attempts[mode] == 1
                # Start without blocking v1 prompt completion, gather user input, then
                # close only its owner. The actual pending Writer request must cancel.
                bridge.next_id += 1
                bridge.send({'jsonrpc':'2.0','id':bridge.next_id,'method':'session/prompt','params':{
                    'sessionId':sid,'prompt':[{'type':'text','text':'pending'}]}})
                while pending is None:
                    handle(bridge.output.get(timeout=15))
                request('session/cancel', {'sessionId':sid})
                deadline = time.monotonic() + 10
                while not cancelled:
                    handle(bridge.output.get(timeout=max(.01, deadline-time.monotonic())))
                assert pending_call is not None
                # Late user approval cannot restart a cancelled continuation.
                bridge.send({'jsonrpc':'2.0','id':pending,'result':{'action':'accept','content':{'count':1}}})
                prompt(sibling, 'sibling-success')
                assert attempts['pending'] == 1 and attempts['sibling-success'] == 1
                request('session/close', {'sessionId':sid})
                request('session/close', {'sessionId':sibling})
                assert len(elicitations) == 5, elicitations
                print(f'ACP MCP v{version}: multiround form/URL, roots, verbatim decline/cancel, state clearing, invalid/state-only rejection, pending cancellation, sibling isolation PASS')
            finally:
                bridge.close()

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bridge', type=Path, required=True)
    parser.add_argument('--rpc', type=Path, required=True)
    args = parser.parse_args()
    run(args.bridge.resolve(), args.rpc.resolve())
