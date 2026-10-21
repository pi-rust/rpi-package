"""POSIX fault injection for snapshot eviction, reopen races and fork identities."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--web', required=True)
args = parser.parse_args()
if os.name != 'posix':
    raise SystemExit('Run this check on Linux/WSL; the RPC fixture is a POSIX executable.')

with tempfile.TemporaryDirectory(prefix='rpi-event-check-') as temporary:
    root = Path(temporary)
    host = root / 'fault-host'
    host.write_text('''#!/usr/bin/env python3
import json, os, sys, time, uuid
from pathlib import Path
root = Path(__file__).parent
(root / 'starting').write_text(str(os.getpid()))
while (root / 'hold').exists(): time.sleep(.01)
def emit(value): print(json.dumps(value), flush=True)
emit({'type':'ready','protocol':'rpi'})
if '--fork' in sys.argv:
    sid = sys.argv[sys.argv.index('--fork') + 1] if (root / 'collision').exists() else str(uuid.uuid4())
else: sid = sys.argv[sys.argv.index('--session-id') + 1]
for line in sys.stdin:
    command = json.loads(line)
    kind = command['type']
    fixture = command.get('fixture')
    if fixture == 'running': emit({'type':'agent_start'})
    if fixture in ('dialogs','parallel_dialogs'):
        for dialog_id in ['old','current']:
            emit({'type':'ui_dialog_request','requestId':dialog_id,'ui':{'question':dialog_id}})
        if fixture == 'dialogs': emit({'type':'ui_dialog_closed','requestId':'old'})
    if fixture == 'closed': emit({'type':'ui_dialog_closed','requestId':'current'})
    if fixture == 'close_old': emit({'type':'ui_dialog_closed','requestId':'old'})
    if fixture == 'settled': emit({'type':'agent_settled','outcome':'completed'})
    if kind == 'get_messages' and (root / 'burst').exists():
        (root / 'burst').unlink()
        for index in range(600): emit({'type':'message_update','message':{'role':'assistant','timestamp':1,'content':[{'type':'text','text':str(index)}]}})
    data = {'accepted':True} if kind in ('compact','prompt','resume_pending') else {'sessionId':sid} if kind == 'get_state' else {'messages':[], 'total':0, 'hasMore':False} if kind == 'get_messages' else {'records':[], 'hasMore':False}
    emit({'type':'response','id':command['id'],'command':kind,'success':True,'data':data})
    if kind == 'stop': break
''', encoding='utf-8')
    host.chmod(0o700)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    env = dict(os.environ)
    env.pop('RPI_WEB_PASSWORD', None)
    env['RPI_WEB_ALLOWED_HOSTS'] = f'127.0.0.1:{port}'

    def request(path, data=None):
        req = urllib.request.Request(base + path, None if data is None else json.dumps(data).encode(),
            {'x-rpi-web':'1', 'content-type':'application/json'})
        with urllib.request.urlopen(req, timeout=20) as response:
            return json.load(response)

    def until(predicate):
        for _ in range(100):
            if predicate(): return
            time.sleep(.03)
        raise AssertionError('fault check deadline exceeded')

    with open(root / 'bridge.log', 'w') as log:
        process = subprocess.Popen([args.web, '--project', str(root), '--rpi-bin', str(host),
            '--port', str(port), '--registry', str(root / 'registry.json'),
            '--projects-root', str(root / 'projects')], env=env, stdout=log, stderr=log, start_new_session=True)
        try:
            def healthy():
                if process.poll() is not None: raise AssertionError('isolated bridge exited')
                try: return request('/api/v1/health')['ok']
                except OSError: return False
            until(healthy)
            sid = request('/api/v1/sessions', {})['sessionId']
            path = '/api/v1/sessions/' + sid
            (root / 'burst').touch()
            try:
                request(path + '/snapshot')
                raise AssertionError('snapshot silently dropped evicted events')
            except urllib.error.HTTPError as error:
                assert error.code == 409
                assert json.load(error)['code'] == 'snapshot_events_evicted'
            snapshot = request(path + '/snapshot')
            assert snapshot['lastSeq'] == 600 and snapshot['events'] == []
            assert request(path + '/commands', {'type':'compact'})['data']['accepted']
            assert request(path)['status'] == 'running'
            assert request(path + '/snapshot')['status'] == 'running'
            request(path + '/commands', {'type':'ping','fixture':'settled'})
            request(path + '/commands', {'type':'ping','fixture':'running'})
            snapshot = request(path + '/snapshot')
            assert snapshot['status'] == 'running' and snapshot['events'] == []
            request(path + '/commands', {'type':'ping','fixture':'parallel_dialogs'})
            snapshot = request(path + '/snapshot')
            assert [dialog['requestId'] for dialog in snapshot['pendingDialogs']] == ['old','current']
            assert snapshot['pendingDialog']['requestId'] == 'old'
            request(path + '/commands', {'type':'ping','fixture':'closed'})
            snapshot = request(path + '/snapshot')
            assert snapshot['status'] == 'awaiting_input' and snapshot['pendingDialog']['requestId'] == 'old'
            request(path + '/commands', {'type':'ping','fixture':'close_old'})
            assert request(path + '/snapshot')['status'] == 'running'
            request(path + '/commands', {'type':'ping','fixture':'dialogs'})
            snapshot = request(path + '/snapshot')
            assert snapshot['status'] == 'awaiting_input' and snapshot['pendingDialog']['requestId'] == 'current'
            request(path + '/commands', {'type':'ping','fixture':'closed'})
            snapshot = request(path + '/snapshot')
            assert snapshot['status'] == 'running' and snapshot['pendingDialog'] is None
            request(path + '/commands', {'type':'ping','fixture':'settled'})
            assert request(path + '/snapshot')['status'] == 'ready'
            request(path + '/rename', {'title':'Before reopen'})
            request(path + '/commands', {'type':'stop'})
            until(lambda: request(path)['status'] == 'closed')
            (root / 'starting').unlink()
            (root / 'hold').touch()
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(request, path + '/open', {})
                until(lambda: (root / 'starting').exists())
                request(path + '/rename', {'title':'Renamed while starting'})
                (root / 'hold').unlink()
                assert future.result()['title'] == 'Renamed while starting'
            assert request(path)['title'] == 'Renamed while starting'
            (root / 'collision').touch()
            try:
                request(path + '/clone', {'title':'Must not replace source'})
                raise AssertionError('fork identity collision was accepted')
            except urllib.error.HTTPError as error:
                assert error.code == 502
            assert len(request('/api/v1/sessions')['sessions']) == 1
            assert request(path)['title'] == 'Renamed while starting'
            assert request(path + '/commands', {'type':'get_state'})['data']['sessionId'] == sid
            print(json.dumps({'passed':True, 'snapshotEvictionDetected':True, 'snapshotRetry':True,
                'concurrentRenamePreserved':True, 'forkCollisionRejected':True, 'sourceHostPreserved':True,
                'snapshotBoundaryStatus':True, 'consecutiveDialogsPreserved':True, 'parallelDialogsPreserved':True,
                'acceptedCompactionIsRunning':True}))
        finally:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=10)
