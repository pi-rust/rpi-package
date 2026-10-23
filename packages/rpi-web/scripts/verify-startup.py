"""Verify the terminal server page and its advertised HTTP endpoint without model calls."""
import argparse
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
import urllib.request
from urllib.parse import urlsplit

parser = argparse.ArgumentParser()
parser.add_argument('--web', required=True)
parser.add_argument('--rpi')
parser.add_argument('--plugin')
args = parser.parse_args()
assert bool(args.rpi) == bool(args.plugin), '--rpi and --plugin must be supplied together'
web = str(Path(args.web).resolve())
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
checked = []
with tempfile.TemporaryDirectory(prefix='rpi-web-startup-') as directory:
    root = Path(directory)
    agent = root / 'agent'
    extensions = agent / 'extensions'
    extensions.mkdir(parents=True)
    (root / '.rpi').mkdir()
    env = dict(os.environ, RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(extensions),
               RPI_OFFLINE='1', ANTHROPIC_API_KEY='fixture-only')
    env.pop('RPI_WEB_ALLOWED_HOSTS', None)
    env.pop('RPI_WEB_PASSWORD', None)
    (agent / 'settings.json').write_text(json.dumps({'defaultProvider': 'anthropic',
        'defaultModel': 'claude-sonnet-4-5'}), encoding='utf-8')
    commands = [('standalone', [web])]
    if args.plugin:
        plugin = Path(args.plugin).resolve()
        shutil.copy2(plugin, extensions / plugin.name)
        for flag in ['--webserver', '--web']:
            commands.append(('extension ' + flag, [str(Path(args.rpi).resolve()), '--offline', '--no-session',
                '--local-only', '--extensions-dir', str(extensions), flag]))
    binds = ['127.0.0.1', '0.0.0.0']
    ipv6 = False
    try:
        with socket.socket(socket.AF_INET6) as listener:
            listener.bind(('::1', 0))
        binds.extend(['::1', '::'])
        ipv6 = True
    except OSError:
        pass

    def settings(bind, port=0):
        (root / '.rpi/web.json').write_text(json.dumps({'project': '..', 'bind': bind, 'port': port,
            'registry': str(root / 'sessions.json')}), encoding='utf-8')

    def cleanup(process):
        if process.poll() is not None:
            return
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        else:
            process.send_signal(signal.SIGINT)
        process.wait(timeout=15)

    for mode, command in commands:
        for bind in binds:
            settings(bind)
            log_path = root / 'server.log'
            with log_path.open('w', encoding='utf-8') as log:
                process = subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.DEVNULL,
                    stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
                try:
                    for _ in range(150):
                        text = log_path.read_text(encoding='utf-8')
                        assert process.poll() is None, text
                        match = re.search(r'Open\s+(http://\S+)', text)
                        if match:
                            break
                        time.sleep(.1)
                    else:
                        raise AssertionError('No startup page: ' + text)
                    url = match[1]
                    port = urlsplit(url).port
                    assert port and port != 0
                    assert 'Status      Running' in text and 'rpi Web Server v' in text
                    assert f'Listen IP   {bind}\n' in text
                    assert f'Port        {port}\n' in text
                    assert 'Ctrl+C' in text and 'Password    Not configured' in text
                    assert not any(char in text for char in ['\x1b', '\r'])
                    assert urlsplit(url).hostname not in ['0.0.0.0', '::']
                    with opener.open(url + '/api/v1/health', timeout=10) as response:
                        assert json.load(response)['ok']
                    with opener.open(url, timeout=10) as response:
                        assert '<html' in response.read().decode().lower()
                    checked.append({'mode': mode, 'bind': bind, 'actualPort': True, 'http': True})
                finally:
                    cleanup(process)
                if os.name != 'nt':
                    text = log_path.read_text(encoding='utf-8')
                    assert 'Status      Stopping...' in text and 'Status      Stopped' in text, text
        with socket.socket() as occupied:
            occupied.bind(('127.0.0.1', 0))
            occupied.listen()
            settings('127.0.0.1', occupied.getsockname()[1])
            failed = subprocess.run(command, cwd=root, env=env, stdin=subprocess.DEVNULL,
                capture_output=True, text=True, encoding='utf-8', timeout=30,
                creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            assert failed.returncode != 0
            assert 'Status      Running' not in failed.stdout
            assert 'cannot listen on' in failed.stderr
print(json.dumps({'passed': True, 'checks': checked, 'ipv6': ipv6,
    'bindFailureHasNoRunningPage': True, 'noModelCalls': True}))
