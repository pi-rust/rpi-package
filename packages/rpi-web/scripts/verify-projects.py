"""Exercise project/chat persistence with an isolated bridge and no model calls."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import socket
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

p = argparse.ArgumentParser()
p.add_argument('--web', required=True)
p.add_argument('--rpi', required=True)
p.add_argument('--project', required=True)
a = p.parse_args()

with tempfile.TemporaryDirectory(prefix='rpi-project-check-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    child = None
    log = open(root / 'bridge.log', 'w', encoding='utf-8')

    def request(path, data=None, method=None):
        req = urllib.request.Request(base + path, None if data is None else json.dumps(data).encode(),
                                     {'x-rpi-web': '1', 'content-type': 'application/json'}, method=method)
        with urllib.request.urlopen(req, timeout=30) as response:
            body = response.read()
            return json.loads(body) if body else None

    def start():
        env = dict(os.environ)
        env.pop('RPI_WEB_PASSWORD', None)
        (root / 'config').mkdir(exist_ok=True)
        env['RPI_CODING_AGENT_DIR'] = str(root / 'config')
        env['ANTHROPIC_API_KEY'] = 'isolated-test-key'
        env['RPI_OFFLINE'] = '1'
        env['RPI_WEB_ALLOWED_HOSTS'] = f'127.0.0.1:{port}'
        process = subprocess.Popen([a.web, '--project', a.project, '--rpi-bin', a.rpi,
                                    '--port', str(port), '--registry', str(root / 'registry.json'),
                                    '--projects-root', str(root / 'projects'), '--session-dir', str(root / 'sessions')],
                                   stdout=log, stderr=log, env=env,
                                   start_new_session=os.name != 'nt',
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        for _ in range(50):
            if process.poll() is not None:
                raise AssertionError('isolated bridge exited')
            try:
                if request('/api/v1/health')['ok']:
                    return process
            except (OSError, urllib.error.URLError):
                time.sleep(.1)
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           creationflags=subprocess.CREATE_NO_WINDOW)
        else:
            os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=10)
        raise AssertionError('isolated bridge did not start')

    def stop(process):
        # Gracefully stop only the hosts created by this isolated test.
        for session in request('/api/v1/sessions')['sessions']:
            try:
                request(f"/api/v1/sessions/{session['sessionId']}/commands", {'type': 'stop'})
            except urllib.error.HTTPError:
                pass  # Remembered offline hosts do not have a command channel.
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], check=False,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           creationflags=subprocess.CREATE_NO_WINDOW)
        else:
            os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=10)

    try:
        child = start()
        initial = request('/api/v1/projects')
        project = request('/api/v1/projects', {'name': '验收项目', 'initGit': True})['project']
        assert Path(project['path']).resolve().is_relative_to((root / 'projects').resolve())
        assert Path(project['path'], '.git').is_dir()
        sid = request('/api/v1/sessions', {'projectId': project['id']})['sessionId']
        path = '/api/v1/sessions/' + sid
        assert request(path + '/commands', {'type': 'get_state'})['success']
        for endpoint, body in [('/api/v1/sessions', b'{'),
                               ('/api/v1/sessions', b'{"projectId":42}'),
                               (path + '/clone', b'{"entryId":42}')]:
            invalid = urllib.request.Request(base + endpoint, body,
                {'x-rpi-web': '1', 'content-type': 'application/json'}, method='POST')
            try:
                urllib.request.urlopen(invalid, timeout=20)
                raise AssertionError('invalid creation body was silently accepted')
            except urllib.error.HTTPError as error:
                assert error.code == 400
        assert len(request('/api/v1/sessions')['sessions']) == 1
        request(path + '/rename', {'title': 'Project task'})
        request(path + '/metadata', {'pinned': True, 'archived': True})
        item = next(s for s in request('/api/v1/sessions')['sessions'] if s['sessionId'] == sid)
        assert item['projectId'] == project['id'] and item['title'] == 'Project task'
        assert request(path)['title'] == 'Project task'
        assert item['pinned'] and item['archived']
        assert request('/api/v1/projects/' + project['id'] + '/git/status')['isRepository']
        probe = request('/api/v1/sessions', {'projectId': project['id']})['sessionId']
        blocked = root / 'registry.json.tmp'
        blocked.mkdir()
        try:
            for failed_path, data, method in [
                (path + '/rename', {'title': 'Must not be saved'}, None),
                (path, None, 'DELETE'),
                ('/api/v1/sessions', {'projectId': project['id']}, None),
                ('/api/v1/sessions/' + probe + '/commands', {'type': 'prompt', 'message': 'Must not invoke a provider'}, None),
            ]:
                try:
                    request(failed_path, data, method)
                    raise AssertionError('failed registry write was reported as successful')
                except urllib.error.HTTPError as error:
                    assert error.code == 500
            probe_state = request('/api/v1/sessions/' + probe + '/commands', {'type': 'get_state'})
            assert not probe_state['data'].get('activeOperation'), 'prompt ran after its persistence failed'
            remaining = request('/api/v1/sessions')['sessions']
            assert len(remaining) == 2
            assert next(s for s in remaining if s['sessionId'] == sid)['title'] == 'Project task'
        finally:
            blocked.rmdir()
        request('/api/v1/sessions/' + probe, method='DELETE')
        for payload in [{'name': '../escape'}, {'name': 'Outside', 'path': str(root)}, {'name': '验收项目'}]:
            try:
                request('/api/v1/projects', payload)
                raise AssertionError('invalid or duplicate project was accepted')
            except urllib.error.HTTPError as error:
                assert error.code == 400
        stop(child)
        child = start()
        assert any(p['id'] == project['id'] for p in request('/api/v1/projects')['projects'])
        restored = next(s for s in request('/api/v1/sessions')['sessions'] if s['sessionId'] == sid)
        assert restored['pinned'] and restored['archived'] and restored['projectId'] == project['id']
        assert restored['title'] == 'Project task'
        request(path + '/metadata', {'archived': False})
        with ThreadPoolExecutor(max_workers=8) as pool:
            opened = list(pool.map(lambda _: request(path + '/open', {}), range(8)))
        assert all(item['title'] == 'Project task' for item in opened)
        assert request(path)['title'] == 'Project task'
        assert request(path)['createdAt'] == restored['createdAt']
        if os.name == 'nt':
            count = subprocess.check_output(['powershell', '-NoProfile', '-NonInteractive', '-Command',
                    f"@(Get-CimInstance Win32_Process -Filter 'ParentProcessId = {child.pid}' | Where-Object Name -eq rpi.exe).Count"],
                    text=True, creationflags=subprocess.CREATE_NO_WINDOW)
            assert int(count.strip()) == 1, 'concurrent reopen created multiple native hosts'
        else:
            children = subprocess.check_output(['ps', '-o', 'pid=', '--ppid', str(child.pid)], text=True).split()
            assert len(children) == 1, 'concurrent reopen created multiple native hosts'
        assert request(path + '/commands', {'type': 'get_state'})['success']
        assert request('/api/v1/projects')['defaultProjectId'] == initial['defaultProjectId']
        calls = [{'type': command, 'id': caller_id} for command in ['get_state', 'get_history', 'get_stats', 'get_queue']
                 for caller_id in ['shared-caller-id', None]]
        with ThreadPoolExecutor(max_workers=8) as pool:
            replies = list(pool.map(lambda call: request(path + '/commands', call), calls))
        for call, reply in zip(calls, replies):
            assert reply['success'] and reply['command'] == call['type'] and (
                reply['id'] == call['id'] if call['id'] is not None else isinstance(reply['id'], str)), {
                'call': call, 'reply': {key: reply.get(key) for key in ['id', 'command', 'success', 'error']}}
        cloned = request(path + '/clone', {'title': 'Clone validation'})['sessionId']
        assert cloned != sid
        clone_path = '/api/v1/sessions/' + cloned
        assert request(clone_path + '/commands', {'type':'get_state'})['data']['sessionId'] == cloned
        clone_entry = next(s for s in request('/api/v1/sessions')['sessions'] if s['sessionId'] == cloned)
        assert clone_entry['projectId'] == project['id'] and clone_entry['parentSessionId'] == sid
        try:
            request(path + '/fork', {'entryId':'missing-entry-for-validation'})
            raise AssertionError('invalid fork target was accepted')
        except urllib.error.HTTPError as error:
            assert error.code == 400
        assert request(path)['title'] == 'Project task'
        request(clone_path, method='DELETE')
        print(json.dumps({'passed': True, 'projectCreation': True, 'gitInitialization': True,
                          'sessionProjectBinding': True, 'renameAndOrganization': True,
                          'restartAndReopen': True, 'concurrentReopen': True,
                          'persistenceFailuresRejected': True, 'failedPromptNotStarted': True,
                          'invalidPathsRejected': True, 'concurrentRequestIdsIsolated': True,
                          'malformedCreationRejected': True, 'validClonePreservesSource': True,
                          'invalidForkRejected': True}))
    finally:
        if child and child.poll() is None:
            stop(child)
        log.close()
