#!/usr/bin/env python3
"""Real native unread-stdout regression. No builds/installs; stdlib only.

Retains stdout's read handle OPEN without reading after ready. Popen owns an
immutable Windows process handle (wait/kill never rediscover a PID). All files
are unique C: artifacts. Saturated cancellation cannot promise a delivered ack;
provider TCP closure is observed separately, then fatal process exit is required.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import threading
import time
import uuid
from sampling_native import ModelServer, Native, environment, sample_request, require, dump


def run_case(binary, root, mode):
    root.mkdir()
    server = ModelServer()
    state = {'received': threading.Event(), 'closed': threading.Event()}
    marker = 'BACKPRESSURE_' + uuid.uuid4().hex
    server.stalls[marker] = state
    process = None
    sender = None
    errors = []
    result = {'mode': mode}
    try:
        env, workspace = environment(root, server)
        if mode == 'drained_cancel':
            native = Native(binary, root, env, workspace)
            process = native.process
            native.receive(lambda f: f.get('type') == 'ready', 20)
            sid = str(uuid.uuid4())
            rid = native.send('sample', sampleId=sid, request=sample_request(marker), timeoutMs=30000)
            require(state['received'].wait(10), 'provider did not start')
            ack = native.request('cancel_sample', sampleId=sid)
            terminal = native.response(rid)
            require(ack.get('success') is True and ack['data']['sampleId'] == sid, 'exact cancellation ack missing')
            require(terminal.get('success') is False, 'cancelled sample succeeded')
            require(state['closed'].wait(2), 'provider connection did not close')
            native.close()
            result.update(exitCode=process.returncode, exactCancel=True, providerClosed=True)
            require(process.returncode == 0, 'drained cancellation must exit normally')
        else:
            stderr = (root / 'stderr.log').open('wb')
            process = subprocess.Popen([str(binary), '--mode', 'rpc', '--session-id', str(uuid.uuid4()),
                '--session-dir', str(root / 'sessions'), '--provider', 'local', '--model', 'fixture'],
                cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0)
            # Read precisely one ready line, with bounded startup and no reader
            # left behind to accidentally drain the stalled pipe.
            ready = []
            reader = threading.Thread(target=lambda: ready.append(process.stdout.readline()), daemon=True)
            reader.start()
            reader.join(20)
            require(not reader.is_alive() and json.loads(ready[0])['type'] == 'ready', 'ready timeout')
            def send(value):
                data = (json.dumps(value) + '\n').encode()
                while data:
                    n = process.stdin.write(data)
                    require(n, 'stdin write returned zero')
                    data = data[n:]
            sid = str(uuid.uuid4())
            send(dict(type='sample', id='active-sample', sampleId=sid, request=sample_request(marker), timeoutMs=30000))
            require(state['received'].wait(10), 'provider did not start')
            started = time.monotonic()
            delivered = threading.Event()
            def flood():
                try:
                    # First response cannot fit in the OS pipe; the next 64
                    # occupy every queue slot. No stdout reader exists now.
                    for i in range(65):
                        send(dict(type='ping', id=str(i) + '_' + 'x' * 131072))
                    if mode == 'cancel':
                        send(dict(type='cancel_sample', id='exact-cancel', sampleId=sid))
                    elif mode == 'command':
                        send(dict(type='get_state', id='overflow'))
                    delivered.set()
                    if mode == 'eof':
                        process.stdin.close()
                except (OSError, ValueError) as exc:
                    errors.append(type(exc).__name__)
            sender = threading.Thread(target=flood, daemon=True)
            sender.start()
            require(delivered.wait(10), 'full-queue control was not sent')
            require(state['closed'].wait(4), 'provider did not close under backpressure')
            provider_closed_at = time.monotonic() - started
            code = process.wait(timeout=7)  # deliberately DO NOT communicate/read stdout
            elapsed = time.monotonic() - started
            sender.join(2)
            require(not sender.is_alive(), 'stdin producer still live')
            require(code == 1, 'fatal stalled sink must exit with status 1')
            require(elapsed < 6, 'fatal escalation exceeded grace')
            require(not errors, 'control producer failed: ' + str(errors))
            stderr.close()
            require('fatal RPC output/producer shutdown stall' in (root / 'stderr.log').read_text(), 'missing actual CLI escalation marker')
            result.update(exitCode=code, seconds=elapsed, providerClosedSeconds=provider_closed_at,
                          stdoutKeptOpenUnread=True, controlSent=True,
                          cancelSampleId=sid if mode == 'cancel' else None,
                          acknowledgement='not observable on failed stdout; no joined-ack claim')
        require(not server.failures, 'provider fixture failures: ' + str(server.failures))
        return result
    finally:
        if process is not None:
            if process.poll() is None:
                process.kill()  # only this test-owned kernel process handle
                process.wait(timeout=5)
            if sender:
                sender.join(2)
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream and not stream.closed:
                    stream.close()
        server.shutdown()
        server.server_close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--expected-sha256', required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    args = parser.parse_args()
    binary, root = args.native.resolve(), args.artifacts.resolve()
    require(root.drive.upper() == 'C:', 'C: artifacts required')
    root.mkdir(parents=True, exist_ok=False)
    sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    require(sha == args.expected_sha256, 'binary SHA mismatch')
    report = {'native': str(binary), 'sha256': sha, 'checks': []}
    try:
        for mode in ('drained_cancel', 'cancel', 'eof', 'command'):
            try:
                report['checks'].append(dict(status='pass', **run_case(binary, root / mode, mode)))
            except Exception as exc:
                report['checks'].append(dict(mode=mode, status='fail', error=str(exc)))
        require(all(c['status'] == 'pass' for c in report['checks']), 'native backpressure checks failed')
    finally:
        dump(root / 'results.json', report)
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
