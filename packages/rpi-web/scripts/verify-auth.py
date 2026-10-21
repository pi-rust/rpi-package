"""Verify real HTTP login/logout and revocation of an open SSE connection, without model calls."""
import argparse
import http.client
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

with tempfile.TemporaryDirectory(prefix='rpi-auth-check-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    jar = http.cookiejar.CookieJar()
    client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    password = 'isolated-test-' + os.urandom(16).hex()

    def request(path, data=None, method=None):
        req = urllib.request.Request(base + path, None if data is None else json.dumps(data).encode(),
                                     {'x-rpi-web': '1', 'content-type': 'application/json'}, method=method)
        return client.open(req, timeout=20)

    def payload(path, data=None, method=None):
        with request(path, data, method) as response:
            body = response.read()
            return json.loads(body) if body else None

    env = dict(os.environ)
    (root / 'config').mkdir()
    env['RPI_CODING_AGENT_DIR'] = str(root / 'config')
    env['ANTHROPIC_API_KEY'] = 'isolated-test-key'
    env['RPI_OFFLINE'] = '1'
    env['RPI_WEB_PASSWORD'] = password
    env['RPI_WEB_ALLOWED_HOSTS'] = f'127.0.0.1:{port}'
    connection = None
    with open(root / 'bridge.log', 'w', encoding='utf-8') as log:
        process = subprocess.Popen([args.web, '--project', str(root), '--rpi-bin', args.rpi,
                                    '--port', str(port), '--registry', str(root / 'registry.json'),
                                    '--projects-root', str(root / 'projects'), '--session-dir', str(root / 'sessions')],
                                   env=env, stdout=log, stderr=log, start_new_session=os.name != 'nt',
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        try:
            for _ in range(50):
                if process.poll() is not None:
                    raise AssertionError('isolated bridge exited')
                try:
                    if payload('/api/v1/health')['ok']:
                        break
                except OSError:
                    time.sleep(.1)
            else:
                raise AssertionError('isolated bridge did not start')
            try:
                payload('/api/v1/sessions')
                raise AssertionError('protected endpoint was public')
            except urllib.error.HTTPError as error:
                assert error.code == 401
            payload('/api/v1/auth', {'password': password})
            assert payload('/api/v1/auth')['authenticated']
            sid = payload('/api/v1/sessions', {})['sessionId']
            old_cookie = '; '.join(f'{cookie.name}={cookie.value}' for cookie in jar)
            connection = http.client.HTTPConnection('127.0.0.1', port, timeout=15)
            connection.request('GET', f'/api/v1/sessions/{sid}/events?after=0', headers={'Cookie': old_cookie})
            stream = connection.getresponse()
            assert stream.status == 200
            first_event = []
            while True:
                line = stream.readline()
                assert line, 'event stream closed before ready'
                if line == b'\n':
                    break
                first_event.append(line)
            assert b'event: ready\n' in first_event
            with request('/api/v1/auth', method='DELETE') as response:
                assert 'Max-Age=0' in response.headers['Set-Cookie']
                response.read()
            assert not list(jar), 'logout cookie was not removed'
            revoked = urllib.request.Request(base + '/api/v1/sessions', headers={'Cookie': old_cookie})
            try:
                urllib.request.urlopen(revoked, timeout=10)
                raise AssertionError('revoked cookie was accepted')
            except urllib.error.HTTPError as error:
                assert error.code == 401
            # A separate login stops the native host and emits an event. The old
            # already-established connection must end without exposing that event.
            payload('/api/v1/auth', {'password': password})
            payload(f'/api/v1/sessions/{sid}/commands', {'type': 'stop'})
            assert stream.read() == b'', 'revoked stream received protected events'
            payload(f'/api/v1/sessions/{sid}', method='DELETE')
            print(json.dumps({'passed': True, 'loginRequired': True, 'logoutCookieCleared': True,
                              'revokedCookieRejected': True, 'openEventStreamRevoked': True}))
        finally:
            if connection:
                connection.close()
            if process.poll() is None:
                if os.name == 'nt':
                    subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'], check=False,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
                else:
                    os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=10)
