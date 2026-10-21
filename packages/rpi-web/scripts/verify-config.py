"""Verify file-only startup, authentication and persistence without a model call."""
import argparse
import http.cookiejar
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
parser.add_argument('--rpi', required=True)
args = parser.parse_args()
web, rpi = str(Path(args.web).resolve()), str(Path(args.rpi).resolve())
assets = Path(__file__).resolve().parents[1] / 'assets'

with tempfile.TemporaryDirectory(prefix='rpi-web-config-check-') as temporary:
    root = Path(temporary)
    project, home = root / 'project with spaces', root / 'home'
    project.mkdir()
    home.mkdir()
    (project / 'README.md').write_text('Configuration check', encoding='utf-8')
    env = dict(os.environ, HOME=str(home), USERPROFILE=str(home),
               RPI_CODING_AGENT_DIR=str(root / 'agent'), RPI_OFFLINE='1',
               ANTHROPIC_API_KEY='isolated-test-key', RPI_WEB_PASSWORD='isolated-web-password')
    env.pop('RPI_WEB_ALLOWED_HOSTS', None)
    Path(env['RPI_CODING_AGENT_DIR']).mkdir()
    subprocess.run([web, '--init'], cwd=project, env=env, check=True, capture_output=True)
    file = project / '.rpi/web.json'
    seed = file.read_bytes()
    assert json.loads(seed)['project'] == '..'
    assert subprocess.run([web, '--init'], cwd=project, env=env, capture_output=True).returncode != 0
    assert file.read_bytes() == seed
    ports = []
    while len(ports) < 2:
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        if port not in ports:
            ports.append(port)
    settings = dict(project='..', rpiBin=rpi, assets=str(assets), port=ports[0],
                    registry='web/sessions.json', sessionDir='web/history', projectsRoot='web/projects',
                    allowedHosts=[f'127.0.0.1:{port}' for port in ports])
    file.write_text(json.dumps(settings), encoding='utf-8')
    child = None
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base = ''

    def request(path, data=None):
        req = urllib.request.Request(base + path, None if data is None else json.dumps(data).encode(),
            {'X-Rpi-Web': '1', 'Content-Type': 'application/json', 'Origin': base})
        with opener.open(req, timeout=20) as response:
            return json.load(response)

    def stop():
        global child
        if child is None:
            return
        if child.poll() is None:
            if os.name == 'nt':
                subprocess.run(['taskkill', '/PID', str(child.pid), '/T', '/F'], check=False,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
            else:
                os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=10)
        child = None

    with (root / 'bridge.log').open('w', encoding='utf-8') as log:
        def start(cwd, port, flags=()):
            global child, base, opener
            base = f'http://127.0.0.1:{port}'
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            child = subprocess.Popen([web, *flags], cwd=cwd, env=env, stdout=log, stderr=log,
                start_new_session=os.name != 'nt', creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            for _ in range(70):
                if child.poll() is not None:
                    raise AssertionError('configured Web exited')
                try:
                    if request('/api/v1/health')['ok']:
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(.1)
            else:
                raise AssertionError('configured Web did not become healthy')
            assert request('/api/v1/auth')['required']
            probe = urllib.request.Request(base + '/api/v1/health', headers={'Host': f'localhost:{port}'})
            try:
                with opener.open(probe, timeout=10) as response:
                    assert env.get('RPI_WEB_ALLOWED_HOSTS') and response.status == 200
            except urllib.error.HTTPError as error:
                assert not env.get('RPI_WEB_ALLOWED_HOSTS') and error.code == 403
            try:
                request('/api/v1/projects')
                raise AssertionError('password protection was bypassed')
            except urllib.error.HTTPError as error:
                assert error.code == 401
            assert request('/api/v1/auth', {'password': env['RPI_WEB_PASSWORD']})['authenticated']
            projects = request('/api/v1/projects')['projects']
            assert any(Path(p['path']).resolve() == project.resolve() for p in projects)

        try:
            start(project, ports[0])
            sid = request('/api/v1/sessions', {})['sessionId']
            path = '/api/v1/sessions/' + sid
            assert request(path + '/commands', {'type': 'get_state'})['success']
            request(path + '/rename', {'title': 'File-only startup'})
            assert (file.parent / 'web/sessions.json').is_file()
            assert (file.parent / 'web/history').is_dir()
            stop()
            start(project, ports[1], ['--port', str(ports[1])])
            restored = next(s for s in request('/api/v1/sessions')['sessions'] if s['sessionId'] == sid)
            assert restored['title'] == 'File-only startup'
            assert request(path + '/open', {})['sessionId'] == sid
            assert request(path + '/commands', {'type': 'get_state'})['success']
            stop()
            global_file = home / '.rpi/web.json'
            global_file.parent.mkdir()
            settings['project'] = str(project)
            for field in ['registry', 'sessionDir', 'projectsRoot']:
                settings[field] = str(file.parent / settings[field])
            global_file.write_text(json.dumps(settings), encoding='utf-8')
            env['RPI_WEB_ALLOWED_HOSTS'] = f'127.0.0.1:{ports[0]},localhost:{ports[0]}'
            start(root, ports[0])
            assert any(s['sessionId'] == sid for s in request('/api/v1/sessions')['sessions'])
            print(json.dumps(dict(passed=True, noModelCalls=True, zeroArgumentStartup=True,
                initPreservesExisting=True, configuredAuthentication=True, configuredHostFilter=True,
                environmentHostOverride=True, cliPortOverride=True,
                relativePaths=True, nativeRpc=True, restartPreservesSessions=True, globalConfig=True)))
        finally:
            stop()
