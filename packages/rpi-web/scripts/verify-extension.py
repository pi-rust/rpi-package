"""Check the installed Web extension with a real native host; no model calls."""
import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--rpi', required=True)
parser.add_argument('--plugin', required=True)
parser.add_argument('--browser', action='store_true')
parser.add_argument('--flag', choices=['--webserver', '--web'], default='--webserver')
args = parser.parse_args()
service_flag = args.flag
host, plugin = str(Path(args.rpi).resolve()), Path(args.plugin).resolve()

with tempfile.TemporaryDirectory(prefix='rpi-web-extension-check-') as temporary:
    root = Path(temporary)
    extensions = root / 'agent/extensions'
    extensions.mkdir(parents=True)
    shutil.copy2(plugin, extensions / plugin.name)
    (root / 'README.md').write_text('Web extension check', encoding='utf-8')
    env = dict(os.environ, HOME=str(root), USERPROFILE=str(root),
               RPI_CODING_AGENT_DIR=str(root / 'agent'), RPI_EXTENSIONS_DIR=str(extensions),
               RPI_OFFLINE='1', ANTHROPIC_API_KEY='fixture-only', RPI_WEB_PASSWORD='fixture-access')
    env.pop('RPI_WEB_ALLOWED_HOSTS', None)
    command = [host, '--offline', '--no-session', '--local-only', '--provider', 'anthropic',
               '--model', 'claude-sonnet-4-5', '--extensions-dir', str(extensions)]
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    config = root / '.rpi/web.json'
    result = subprocess.run([*command, service_flag + '=init'], cwd=root, env=env, stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, encoding='utf-8', timeout=30)
    assert result.returncode == 0, result.stderr
    assert json.loads(config.read_text(encoding='utf-8'))['project'] == '..'
    original = config.read_bytes()
    repeated = subprocess.run([*command, service_flag + '=init'], cwd=root, env=env, stdin=subprocess.DEVNULL,
                              capture_output=True, timeout=30)
    assert repeated.returncode == 2 and config.read_bytes() == original
    help_result = subprocess.run([*command, service_flag + '=help'], cwd=root, env=env,
        stdin=subprocess.DEVNULL, capture_output=True, text=True, encoding='utf-8', timeout=30)
    assert help_result.returncode == 0 and 'rpi --webserver' in help_result.stdout, help_result.stderr
    settings = dict(project='..', bind='0.0.0.0', port=0, registry='web/sessions.json', sessionDir='web/history')
    config.write_text(json.dumps(settings), encoding='utf-8')
    process = None
    opener = None

    def request(path, data=None, raw=False):
        req = urllib.request.Request(base + path, None if data is None else json.dumps(data).encode(),
            {'X-Rpi-Web': '1', 'Content-Type': 'application/json', 'Origin': base})
        with opener.open(req, timeout=20) as response:
            body = response.read()
            return body if raw else json.loads(body)

    with (root / 'web.log').open('w', encoding='utf-8') as log:
        def start():
            global process, opener
            opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
            process = subprocess.Popen([*command, service_flag + '=' + str(config), '--bind', '127.0.0.1', '--port', str(port)], cwd=root, env=env, stdin=subprocess.DEVNULL,
                stdout=log, stderr=log, start_new_session=os.name != 'nt',
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError((root / 'web.log').read_text(encoding='utf-8'))
                try:
                    if request('/api/v1/health')['ok']:
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(.1)
            else:
                raise AssertionError('Web extension startup timed out')
            assert request('/api/v1/auth')['required']
            assert request('/api/v1/auth', {'password': 'fixture-access'})['authenticated']

        def stop():
            global process
            if process is None or process.poll() is not None:
                return
            if os.name == 'nt':
                # SIGTERM bypasses the Windows console lifecycle. Close this
                # fixture's RPC children first; POSIX below tests real SIGINT.
                for session in request('/api/v1/sessions')['sessions']:
                    if session['status'] != 'offline':
                        request('/api/v1/sessions/' + session['sessionId'] + '/commands', {'type': 'stop'})
                process.terminate()
            else:
                process.send_signal(signal.SIGINT)
            process.wait(timeout=15)
            process = None

        try:
            start()
            html = request('/', raw=True).decode()
            references = re.findall(r'(?:src|href)="(/assets/[^\"]+)"', html)
            assert references
            for path in references:
                assert request(path, raw=True), path
            assert request('/favicon.svg', raw=True)
            sid = request('/api/v1/sessions', {})['sessionId']
            path = '/api/v1/sessions/' + sid
            assert request(path + '/commands', {'type': 'get_state'})['success']
            request(path + '/rename', {'title': 'Extension session'})
            if args.browser:
                browser = r'''
const path = require('node:path');
const {chromium, expect} = require(path.resolve(process.argv[1], 'node_modules/@playwright/test'));
(async () => {
 const browser = await chromium.launch({headless:true});
 try {
  const context = await browser.newContext();
  await context.addInitScript(() => localStorage.setItem('rpi-web-language','en'));
  const page = await context.newPage(); const errors=[];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if(message.type()==='error') errors.push(message.text()); });
  await page.goto(process.argv[2]+'/?session='+process.argv[3]);
  await page.locator('#workspace-password').fill('fixture-access');
  await page.locator('.login-form button[type="submit"]').click();
  await expect(page.locator('.composer textarea')).toBeVisible();
  await expect(page.locator('.connection')).toContainText('ready');
  await page.reload();
  await expect(page.locator('.connection')).toContainText('ready');
  expect(errors).toEqual([]);
 } finally { await browser.close(); }
})().catch(error => {console.error(error);process.exitCode=1;});
'''
                subprocess.run(['node', '-e', browser, str(Path(__file__).resolve().parents[1] / 'frontend'), base, sid],
                               check=True, capture_output=True, text=True, timeout=60)
            stream = opener.open(urllib.request.Request(base + path + '/events'), timeout=10)
            assert stream.readline(), 'SSE did not open'
            children = []
            if os.name != 'nt':
                children = subprocess.check_output(['ps', '-o', 'pid=', '--ppid', str(process.pid)], text=True).split()
                assert len(children) == 1, 'RPC child started another Web server'
            stop()
            stream.close()
            if os.name != 'nt':
                for pid in children:
                    for _ in range(50):
                        try:
                            os.kill(int(pid), 0)
                        except ProcessLookupError:
                            break
                        time.sleep(.1)
                    else:
                        raise AssertionError('Native RPC child survived graceful shutdown')
            start()
            saved = next(s for s in request('/api/v1/sessions')['sessions'] if s['sessionId'] == sid)
            assert saved['title'] == 'Extension session'
            request(path + '/open', {})
            assert request(path + '/commands', {'type': 'get_state'})['success']
            stop()
            with socket.socket() as occupied:
                if os.name != 'nt':
                    occupied.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                occupied.bind(('127.0.0.1', port))
                occupied.listen()
                failed = subprocess.run([*command, service_flag + '=true', '--bind', '127.0.0.1', '--port', str(port)], cwd=root, env=env, stdin=subprocess.DEVNULL,
                    capture_output=True, timeout=30)
                assert failed.returncode == 1, 'Bind failure did not finish the host lifecycle'
            missing = subprocess.run([*command, service_flag + '=' + str(root / 'missing.json')], cwd=root, env=env,
                stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
            assert missing.returncode == 2
            conflicting = subprocess.run([*command, service_flag, '--server'], cwd=root, env=env,
                stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
            assert conflicting.returncode == 2
            aliases = subprocess.run([*command, '--webserver=init', '--web=help'], cwd=root, env=env,
                stdin=subprocess.DEVNULL, capture_output=True, text=True, encoding='utf-8', timeout=30)
            assert aliases.returncode == 2 and 'different values' in aliases.stderr
            # No service flag means normal RPC, even with the extension installed.
            rpc = subprocess.run([*command, '--mode', 'rpc'], cwd=root, env=env,
                input='{"type":"stop","id":"fixture-stop"}\n', capture_output=True,
                text=True, encoding='utf-8', timeout=30)
            assert rpc.returncode == 0, rpc.stderr
            assert any(json.loads(line).get('type') == 'ready' for line in rpc.stdout.splitlines() if line.startswith('{'))
            print(json.dumps(dict(passed=True, nativePlugin=True, init=True, frontend=True,
                authenticated=True, nativeRpc=True, noRecursiveStartup=True,
                restartPreservesSessions=True, gracefulShutdown=os.name != 'nt',
                startupFailuresExit=True, initPreservesExisting=True, serviceFlag=service_flag, explicitConfig=True, cliOverrides=True, help=True, aliasConflict=True, browser=args.browser, noModelCalls=True)))
        finally:
            stop()
