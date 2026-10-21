"""Real web + native rpi + local Anthropic fixture; optional real Chromium UI.

No external provider is contacted. All credentials, projects and sessions are temporary.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--web', required=True)
parser.add_argument('--rpi', required=True)
parser.add_argument('--browser', action='store_true')
args = parser.parse_args()
package = Path(__file__).resolve().parent.parent

with tempfile.TemporaryDirectory(prefix='rpi-chat-check-') as temporary:
    root = Path(temporary)
    project = root / 'project'
    project.mkdir()
    (project / 'sample.txt').write_text('Local sample file contents.\n', encoding='utf-8')
    config = root / 'config'
    config.mkdir()
    (config / 'settings.json').write_text(json.dumps({'defaultProvider':'anthropic',
        'defaultModel':'claude-haiku-4-5', 'defaultThinkingLevel':'off'}))
    state = {'requests':0, 'completed':0, 'tool':False}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            encoded = json.dumps(state).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            state['requests'] += 1
            if 'Wait for cancellation.' in json.dumps(body.get('messages', [])[-1:]):
                # Keep the request active until the browser can issue abort.
                time.sleep(2)
                self.send_response(499)
                self.end_headers()
                return
            has_result = any(block.get('type') == 'tool_result' for message in body.get('messages', [])
                for block in message.get('content', []) if isinstance(block, dict))
            tool = not state['tool'] and not has_result
            if tool:
                state['tool'] = True
                block = {'type':'tool_use', 'id':'test-bash-1', 'name':'bash', 'input':{}}
                delta = {'type':'input_json_delta', 'partial_json':json.dumps({'command':"sleep 1.2; echo timed-command"})}
            else:
                state['completed'] += 1
                block = {'type':'text', 'text':''}
                delta = {'type':'text_delta', 'text':f'Local fixture reply {state["completed"]}'}
            frames = [
                {'type':'message_start', 'message':{'id':f'msg-{state["requests"]}', 'type':'message',
                    'role':'assistant', 'model':body['model'], 'content':[], 'stop_reason':None,
                    'usage':{'input_tokens':10, 'output_tokens':0}}},
                {'type':'content_block_start', 'index':0, 'content_block':block},
                {'type':'content_block_delta', 'index':0, 'delta':delta},
                {'type':'content_block_stop', 'index':0},
                {'type':'message_delta', 'delta':{'stop_reason':'tool_use' if tool else 'end_turn',
                    'stop_sequence':None}, 'usage':{'output_tokens':5}},
                {'type':'message_stop'},
            ]
            chunks = [f'event: {frame["type"]}\ndata: {json.dumps(frame)}\n\n'.encode() for frame in frames]
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(sum(map(len, chunks))))
            self.send_header('Connection', 'close')
            self.end_headers()
            for chunk in chunks:
                self.wfile.write(chunk)
                self.wfile.flush()
                time.sleep(.02)

    provider = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    env = dict(os.environ)
    env.pop('RPI_WEB_PASSWORD', None)
    env.pop('ANTHROPIC_AUTH_TOKEN', None)
    env['RPI_CODING_AGENT_DIR'] = str(config)
    env['RPI_OFFLINE'] = '1'
    env['ANTHROPIC_API_KEY'] = 'local-fixture-only'
    env['ANTHROPIC_BASE_URL'] = f'http://127.0.0.1:{provider.server_port}'
    env['RPI_WEB_ALLOWED_HOSTS'] = f'127.0.0.1:{port}'
    env['NO_PROXY'] = '127.0.0.1,localhost'
    log = open(root / 'bridge.log', 'w', encoding='utf-8')
    process = None

    def request(path, data=None):
        req = urllib.request.Request(base + path, None if data is None else json.dumps(data).encode(),
            {'x-rpi-web':'1', 'content-type':'application/json'})
        with urllib.request.urlopen(req, timeout=30) as response:
            return json.load(response)

    def until(predicate):
        for _ in range(150):
            if predicate(): return
            time.sleep(.1)
        raise AssertionError('chat check deadline exceeded')

    def start():
        child = subprocess.Popen([args.web, '--project', str(project), '--rpi-bin', args.rpi,
            '--port', str(port), '--registry', str(root / 'registry.json'), '--assets', str(package / 'assets'),
            '--projects-root', str(root / 'projects'), '--session-dir', str(root / 'sessions')],
            stdout=log, stderr=log, env=env, start_new_session=os.name != 'nt',
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        def healthy():
            assert child.poll() is None, 'isolated bridge exited'
            try: return request('/api/v1/health')['ok']
            except OSError: return False
        until(healthy)
        return child

    def stop(child):
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(child.pid), '/T', '/F'], check=False,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        else:
            os.killpg(child.pid, signal.SIGTERM)
        child.wait(timeout=10)

    try:
        process = start()
        compact_sid = request('/api/v1/sessions', {})['sessionId']
        compact_path = '/api/v1/sessions/' + compact_sid
        assert request(compact_path + '/commands', {'type':'compact'})['data']['accepted']
        until(lambda: (request(compact_path).get('latestSettlement') or {}).get('outcome') == 'declined')
        compact_snapshot = request(compact_path + '/snapshot')
        compact_run_id = compact_snapshot['latestSettlement'].get('runId')
        assert compact_run_id, 'compaction settlement has no operation identity'
        assert not compact_snapshot['history']['openOperations'], 'declined compaction left an open operation'
        assert any(record.get('type') == 'operation_finished' and record.get('runId') == compact_run_id
            and record.get('outcome') == 'declined' for record in compact_snapshot['history']['records'])
        assert state['requests'] == 0, 'no-op compaction unexpectedly called the model'
        request_delete = urllib.request.Request(base + compact_path, method='DELETE', headers={'x-rpi-web':'1'})
        with urllib.request.urlopen(request_delete, timeout=10): pass
        sid = request('/api/v1/sessions', {})['sessionId']
        path = '/api/v1/sessions/' + sid
        if args.browser:
            script = r'''
const path = require('node:path');
const { chromium, expect } = require(path.resolve(process.argv[1], 'node_modules/@playwright/test'));
(async () => {
 const browser = await chromium.launch({headless:true});
 try {
  const context = await browser.newContext();
  await context.addInitScript(() => localStorage.setItem('rpi-web-language','en'));
  const first = await context.newPage(); const second = await context.newPage();
  const errors = [];
  for(const page of [first,second]) page.on('pageerror', error => errors.push(error.message));
  for(const page of [first,second]) await page.goto(process.argv[2]+'/?session='+process.argv[3]);
  for(const page of [first,second]) await expect(page.locator('.connection')).not.toContainText('connecting');
  const prompt = page => page.locator('.composer textarea');
  await prompt(first).fill('Read sample.txt and reply.'); await prompt(first).press('Enter');
  for(const page of [first,second]) {
   await expect(page.locator('.message.assistant')).toContainText(['Local fixture reply 1'], {timeout:15000});
   await expect(page.locator('.tool-card.tool-done')).toHaveCount(1);
   await expect(page.locator('.message.user')).toHaveCount(1);
  }
  for(const page of [first,second]) await expect(page.locator('.composer button.danger')).toBeDisabled();
  await prompt(second).fill('Reply for a second turn.'); await prompt(second).press('Enter');
  for(const page of [first,second]) {
   await expect(page.locator('.message.assistant')).toContainText(['Local fixture reply 1','Local fixture reply 2'], {timeout:15000});
   await expect(page.locator('.message.user')).toHaveCount(2);
   await expect(page.locator('.composer button.danger')).toBeDisabled();
  }
  await first.reload();
  await expect(first.locator('.message.assistant')).toContainText(['Local fixture reply 1','Local fixture reply 2'], {timeout:15000});
  await expect(first.locator('.tool-card.tool-done')).toHaveCount(1);
  await expect(first.locator('.transcript-note')).toHaveCount(2);
  await prompt(first).fill('Wait for cancellation.'); await prompt(first).press('Enter');
  await expect.poll(async () => (await (await fetch(process.argv[4])).json()).requests).toBe(4);
  await first.locator('.composer button.danger').click();
  for(const page of [first,second]) {
   await expect(page.locator('.composer button.danger')).toBeDisabled();
   await expect(page.locator('.connection')).toContainText('ready');
   await expect(page.locator('.message.user')).toHaveCount(3);
  }
  expect(errors).toEqual([]);
  console.log(JSON.stringify({browser:true, tabs:2, turns:2, toolResult:true, reload:true, abort:true}));
 } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode=1; });
'''
            result = subprocess.run(['node', '-e', script, str(package / 'frontend'), base, sid,
                f'http://127.0.0.1:{provider.server_port}'],
                capture_output=True, text=True, encoding='utf-8', timeout=70)
            if result.returncode:
                projection = []
                with urllib.request.urlopen(base + path + '/events?after=0', timeout=5) as events:
                    for line in events:
                        if not line.startswith(b'data: '): continue
                        event = json.loads(line[6:])
                        message = event.get('message') or {}
                        projection.append({'type':event.get('type'), 'role':message.get('role'),
                            'messageKeys':list(message), 'runId':event.get('runId')})
                        if event.get('type') == 'agent_settled': break
                raise AssertionError(json.dumps(projection) + '\n' + result.stderr)
            browser_report = json.loads(result.stdout)
        else:
            for index in range(1, 3):
                accepted = request(path + '/commands', {'type':'prompt', 'message':f'Local test turn {index}'})
                assert accepted['success']
                until(lambda: request(path)['status'] == 'ready' and state['completed'] >= index)
            browser_report = {'browser':False}
        until(lambda: request(path)['status'] == 'ready' and state['completed'] == 2)
        before = request(path + '/snapshot')['messages']
        results = [message for message in before if message.get('role') == 'toolResult']
        assert results and all(not message.get('isError') for message in results)
        timing = results[0].get('webTiming')
        assert timing and timing['elapsedMs'] >= 1100 and not timing['running'], timing
        stop(process)
        process = start()
        request(path + '/open', {})
        after = request(path + '/snapshot')['messages']
        # Restoring native optional fields omits null values (e.g. tool details).
        def canonical(value):
            if isinstance(value, dict): return {key:canonical(item) for key,item in value.items() if item is not None}
            if isinstance(value, list): return [canonical(item) for item in value]
            return value
        assert canonical(before) == canonical(after), 'native transcript changed across bridge/native restart'
        assert state['completed'] == 2 and state['requests'] == (4 if args.browser else 3)
        print(json.dumps({'passed':True, 'localProviderOnly':True, 'realNativeToolCall':True, 'bashDurationPreserved':True,
            'twoTurns':True, 'restartPreservesTranscript':True, 'declinedCompactionSettles':True, **browser_report}))
    finally:
        if process and process.poll() is None: stop(process)
        provider.shutdown()
        provider.server_close()
        log.close()
