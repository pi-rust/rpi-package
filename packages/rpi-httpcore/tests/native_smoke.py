"""Headless real-native archive/delete smoke; isolated fixtures, never prompt a model.

Run only after building the HTTP service. --http-sha/--rpi-sha pin provenance;
only hashes and named checks are printed, never logs, argv or transcript content.
"""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


class Windows:
    """Suspended launch -> own kill-on-close Job -> resume; no breakaway."""
    def __init__(self):
        self.k = ctypes.WinDLL('kernel32', use_last_error=True)
        self.n = ctypes.WinDLL('ntdll')
        H, D, P = wintypes.HANDLE, wintypes.DWORD, ctypes.c_void_p
        for name, arguments, result in [
            ('CreateJobObjectW', [P, wintypes.LPCWSTR], H),
            ('SetInformationJobObject', [H, ctypes.c_int, P, D], wintypes.BOOL),
            ('QueryInformationJobObject', [H, ctypes.c_int, P, D, ctypes.POINTER(D)], wintypes.BOOL),
            ('AssignProcessToJobObject', [H, H], wintypes.BOOL),
            ('TerminateJobObject', [H, wintypes.UINT], wintypes.BOOL),
            ('OpenProcess', [D, wintypes.BOOL, D], H),
            ('GetExitCodeProcess', [H, ctypes.POINTER(D)], wintypes.BOOL),
            ('WaitForSingleObject', [H, D], D),
            ('CreateFileW', [wintypes.LPCWSTR, D, D, P, D, D, H], H),
            ('CloseHandle', [H], wintypes.BOOL),
        ]:
            fn = getattr(self.k, name)
            fn.argtypes, fn.restype = arguments, result
        self.n.NtResumeProcess.argtypes = [H]
        self.n.NtResumeProcess.restype = ctypes.c_long

    @staticmethod
    def check(result):
        if not result:
            raise OSError('owned Windows process operation failed')
        return result

    def job(self):
        class Basic(ctypes.Structure):
            _fields_ = [('user', ctypes.c_longlong), ('job', ctypes.c_longlong),
                        ('flags', wintypes.DWORD), ('min', ctypes.c_size_t),
                        ('max', ctypes.c_size_t), ('active', wintypes.DWORD),
                        ('affinity', ctypes.c_size_t), ('priority', wintypes.DWORD),
                        ('scheduling', wintypes.DWORD)]
        class Extended(ctypes.Structure):
            _fields_ = [('basic', Basic), ('io', ctypes.c_ulonglong * 6),
                        ('process_memory', ctypes.c_size_t), ('job_memory', ctypes.c_size_t),
                        ('peak_process', ctypes.c_size_t), ('peak_job', ctypes.c_size_t)]
        handle = self.check(self.k.CreateJobObjectW(None, None))
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        try:
            self.check(self.k.SetInformationJobObject(handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)))
        except BaseException:
            self.k.CloseHandle(handle)
            raise
        return handle

    def wait_empty(self, job):
        class Accounting(ctypes.Structure):
            _fields_ = [('times', ctypes.c_longlong * 4), ('faults', wintypes.DWORD),
                        ('total', wintypes.DWORD), ('active', wintypes.DWORD), ('terminated', wintypes.DWORD)]
        info = Accounting()
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            self.check(self.k.QueryInformationJobObject(job, 1, ctypes.byref(info), ctypes.sizeof(info), None))
            if info.active == 0:
                return
            time.sleep(.05)
        raise AssertionError('owned Job tree did not exit before deadline')


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--http', required=True)
parser.add_argument('--rpi', required=True)
parser.add_argument('--http-sha', help='expected HTTP executable SHA-256')
parser.add_argument('--rpi-sha', help='expected native executable SHA-256')
args = parser.parse_args()
binary, host = Path(args.http).resolve(strict=True), Path(args.rpi).resolve(strict=True)
provenance = {'httpSha': digest(binary), 'rpiSha': digest(host)}
for supplied, actual in [(args.http_sha, provenance['httpSha']), (args.rpi_sha, provenance['rpiSha'])]:
    if supplied is not None and (not re.fullmatch(r'[0-9a-fA-F]{64}', supplied) or supplied.lower() != actual):
        parser.error('executable SHA-256 mismatch or invalid expected hash')

checks = {}
cleanup = []
win = Windows() if os.name == 'nt' else None
with tempfile.TemporaryDirectory(prefix='rpi-http-native-') as directory:
    root = Path(directory).resolve()
    agent = root / 'agent'
    for relative in ['agent/extensions', 'home', 'appdata', 'localappdata', 'tmp', 'workspace', 'config']:
        (root / relative).mkdir(parents=True)
    (agent / 'settings.json').write_text(json.dumps({
        'defaultProvider': 'anthropic', 'defaultModel': 'claude-sonnet-4-5'}), encoding='utf-8')
    # Never copy ambient credentials, proxy/model commands, RPI config or dotenv.
    env = {key: os.environ[key] for key in ['PATH', 'SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT', 'SYSTEMDRIVE']
           if key in os.environ}
    env.update(HOME=str(root / 'home'), USERPROFILE=str(root / 'home'),
               APPDATA=str(root / 'appdata'), LOCALAPPDATA=str(root / 'localappdata'),
               TEMP=str(root / 'tmp'), TMP=str(root / 'tmp'), TMPDIR=str(root / 'tmp'),
               XDG_CONFIG_HOME=str(root / 'home' / '.config'), XDG_DATA_HOME=str(root / 'home' / '.local/share'),
               XDG_CACHE_HOME=str(root / 'home' / '.cache'),
               RPI_CODING_AGENT_DIR=str(agent), RPI_EXTENSIONS_DIR=str(agent / 'extensions'),
               RPI_OFFLINE='1', ANTHROPIC_API_KEY='fixture-only', RPI_HTTP_PASSWORD='fixture-password')
    config = root / 'config' / 'http.json'
    history_root = config.parent / 'history'
    registry = config.parent / 'state' / 'sessions.json'
    config.write_text(json.dumps({'project': '../workspace', 'rpiBin': str(host), 'port': 31337,
                                 'registry': 'state/sessions.json', 'sessionDir': 'history'}), encoding='utf-8')
    process = log = job = None
    fixtures = set()
    native_handles = []
    base = None
    opener = None

    def request(path, data=None, method=None, timeout=15):
        payload = None if data is None else json.dumps(data).encode()
        req = urllib.request.Request(base + path, payload,
            {'Content-Type': 'application/json', 'X-Rpi-Web': '1'}, method=method)
        try:
            with opener.open(req, timeout=timeout) as response:
                return None if response.status == 204 else json.load(response)
        except urllib.error.HTTPError as error:
            code = error.code
            error.close()
            raise AssertionError('unexpected HTTP status: ' + str(code)) from None

    def expect_status(path, status, body=None, method=None):
        req = urllib.request.Request(base + path, body,
            {'Content-Type': 'application/json', 'X-Rpi-Web': '1'}, method=method)
        try:
            with opener.open(req, timeout=15) as response:
                assert response.status == status, 'unexpected HTTP status'
        except urllib.error.HTTPError as error:
            code = error.code
            error.close()
            assert code == status, 'unexpected HTTP status: ' + str(code)

    def sessions():
        return request('/api/v1/sessions')['sessions']

    def entry(sid):
        return next(value for value in sessions() if value['sessionId'] == sid)

    def history(sid):
        # Only enumerate this invocation's fresh history root, then verify header ID.
        matches = list(history_root.glob('**/*_' + sid + '.jsonl'))
        assert len(matches) == 1, 'expected exactly one native history for fixture ID'
        with matches[0].open('rb') as stream:
            header = json.loads(stream.readline(65537))
        assert header['kind'] == 'header' and header['version'] == 4 and header['id'] == sid
        return matches[0]

    def fingerprint(path):
        return path.stat().st_size, digest(path)

    def native_pids(sid):
        """Inspect only direct children of OUR server; return IDs, never argv."""
        assert process is not None and process.poll() is None
        if win:
            ps_env = dict(env, SMOKE_PARENT=str(process.pid), SMOKE_HOST=str(host), SMOKE_SID=sid)
            script = r"""
$ErrorActionPreference = 'Stop'
Get-CimInstance Win32_Process -Filter ('ParentProcessId = ' + $env:SMOKE_PARENT) |
  Where-Object { $_.ExecutablePath -eq $env:SMOKE_HOST -and
    $_.CommandLine -match '--mode\s+rpc' -and
    $_.CommandLine -match '--session(?:-id)?(?:=|\s)' -and
    $_.CommandLine.Contains($env:SMOKE_SID) } |
  ForEach-Object { [Console]::WriteLine($_.ProcessId) }
"""
            powershell = Path(env['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'
            result = subprocess.run([str(powershell), '-NoProfile', '-NonInteractive', '-Command', script],
                cwd=root, env=ps_env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL, timeout=12, creationflags=subprocess.CREATE_NO_WINDOW)
            assert result.returncode == 0, 'owned native PID inspection failed'
            return [int(line) for line in result.stdout.splitlines() if line.strip()]
        result = subprocess.run(['ps', '-ww', '-eo', 'pid=,ppid=,args='], cwd=root, env=env,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=5)
        assert result.returncode == 0, 'owned native PID inspection failed'
        found = []
        for line in result.stdout.splitlines():
            parts = line.split(None, 2)
            if len(parts) != 3 or parts[1] != str(process.pid):
                continue
            tokens = shlex.split(parts[2])
            if not tokens or Path(tokens[0]).resolve() != host:
                continue
            selected = any(flag in ['--session', '--session-id'] and sid in value
                           for flag, value in zip(tokens, tokens[1:]))
            if selected and any(flag == '--mode' and value == 'rpc' for flag, value in zip(tokens, tokens[1:])):
                found.append(int(parts[0]))
        return found

    def observe_native(sid):
        pids = native_pids(sid)
        assert len(pids) == 1, 'expected one direct native child matching fixture session'
        pid = pids[0]
        if win:
            handle = win.check(win.k.OpenProcess(0x1000 | 0x100000, False, pid))
            native_handles.append(handle)
            assert win.k.WaitForSingleObject(handle, 0) == 258, 'native must be positively live'
            return pid, handle
        # Linux start time distinguishes PID reuse; other Unix uses kill(0).
        stat = Path('/proc') / str(pid) / 'stat'
        token = stat.read_text().rsplit(')', 1)[1].split()[19] if stat.exists() else None
        os.kill(pid, 0)
        return pid, token

    def exited(observed):
        pid, token = observed
        if win:
            code = wintypes.DWORD()
            win.check(win.k.GetExitCodeProcess(token, ctypes.byref(code)))
            return win.k.WaitForSingleObject(token, 0) == 0 and code.value != 259
        try:
            os.kill(pid, 0)
            stat = Path('/proc') / str(pid) / 'stat'
            if token is not None and stat.exists():
                fields = stat.read_text().rsplit(')', 1)[1].split()
                return fields[0] == 'Z' or fields[19] != token
            return False
        except ProcessLookupError:
            return True

    def assert_exited(observed):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if exited(observed):
                return
            time.sleep(.05)
        raise AssertionError('owned native did not exit before deadline')

    def start():
        global process, log, job, base, opener
        base = None
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        log = (root / 'server.log').open('w', encoding='utf-8')
        if win:
            job = win.job()
        process = subprocess.Popen([str(binary), '--port', '0', '--config', str(config.relative_to(root))],
            cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            creationflags=(0x4 | subprocess.CREATE_NO_WINDOW) if win else 0,
            start_new_session=not win)
        if win:
            win.check(win.k.AssignProcessToJobObject(job, int(process._handle)))
            assert win.n.NtResumeProcess(int(process._handle)) == 0, 'owned server resume failed'
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            assert process.poll() is None, 'HTTP server exited during startup'
            text = (root / 'server.log').read_text(encoding='utf-8', errors='replace')
            match = re.search(r'listening on (127\.0\.0\.1:\d+)', text)
            if match:
                base = 'http://' + match[1]
                break
            time.sleep(.1)
        else:
            raise AssertionError('HTTP startup timed out')
        assert request('/api/v1/health')['service'] == 'rpi-http'
        expect_status('/api/v1/sessions', 401)
        assert request('/api/v1/auth', {'password': 'fixture-password'})['authenticated']

    def stop(preserve=()):
        """DELETE first, even archived fixtures. Only restart may preserve IDs."""
        global process, log, job, base
        failures = 0
        try:
            if process is not None and process.poll() is None and base is not None:
                for sid in sorted(fixtures - set(preserve)):
                    try:
                        request('/api/v1/sessions/' + sid, method='DELETE', timeout=5)
                    except Exception:
                        failures += 1  # Server failure must never skip owned-tree cleanup.
                for sid in preserve:
                    try:
                        request('/api/v1/sessions/' + sid + '/metadata', {'archived': True}, timeout=5)
                    except Exception:
                        failures += 1
        finally:
            try:
                if win and job is not None:
                    win.check(win.k.TerminateJobObject(job, 92))
                elif process is not None:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if process is not None:
                    # Includes suspended launch/Job assignment failure, before inheritance.
                    if win and process.poll() is None:
                        process.kill()
                    process.wait(timeout=8)
                if win and job is not None:
                    win.wait_empty(job)
            finally:
                if win and job is not None:
                    win.check(win.k.CloseHandle(job))
                    job = None
                if log is not None:
                    log.close()
                    log = None
                process = None
                base = None
                cleanup.append({'ownedTreeClosed': True, 'deleteFailures': failures,
                                'preservedForRestart': len(preserve)})

    try:
        start()
        expect_status('/', 404)
        expect_status('/assets/index.js', 404)
        expect_status('/api/v1/sessions', 400, b'{')
        assert 'prompt' in request('/api/v1/capabilities')['supportedCommands']
        checks.update(headless=True, authentication=True, isolatedEnvironment=True)
        sid = request('/api/v1/sessions', {})['sessionId']
        fixtures.add(sid)
        path = '/api/v1/sessions/' + sid
        assert request(path + '/commands', {'type': 'get_state'})['success']
        main = history(sid)
        assert not (root / 'workspace' / 'config').exists(), 'config paths reinterpreted under child cwd'
        request(path + '/rename', {'title': 'Shared HTTP session'})
        request(path + '/metadata', {'tags': ['http', 'shared'], 'pinned': True})
        assert request(path + '/snapshot')['sessionId'] == sid
        with opener.open(base + path + '/events', timeout=10) as stream:
            assert stream.readline().startswith(b'event: ready')
        checks.update(nativeRpc=True, snapshot=True, sse=True, relativeConfigPaths=True)
        transient = request('/api/v1/sessions', {})['sessionId']
        fixtures.add(transient)
        transient_path = '/api/v1/sessions/' + transient
        with opener.open(base + transient_path + '/events', timeout=10) as stream:
            assert stream.readline().startswith(b'event: ready')
            request(transient_path, method='DELETE')
            fixtures.remove(transient)
            for _ in range(64):
                line = stream.readline()
                if line.startswith(b'event: host_closed'):
                    break
                assert line, 'SSE ended before delivering host_closed'
            else:
                raise AssertionError('SSE did not publish host_closed')
        checks['sessionCloseEvent'] = True
        # Restart is the sole preserve path; archive releases native file locks first.
        stop(preserve={sid})
        start()
        session = entry(sid)
        assert session['title'] == 'Shared HTTP session' and session['pinned']
        assert session['tags'] == ['http', 'shared']
        assert session['archived'] and session['status'] == 'offline'
        request(path + '/metadata', {'archived': False})
        request(path + '/open', {})
        assert request(path + '/commands', {'type': 'get_state'})['success']
        assert history(sid) == main
        checks.update(restart=True, metadata=True)

        child = request(path + '/clone', {'title': 'Synthetic clone child'})['sessionId']
        fixtures.add(child)
        child_path = '/api/v1/sessions/' + child
        assert child != sid
        assert request(child_path + '/commands', {'type': 'get_state'})['success']
        child_history = history(child)
        assert child_history != main
        assert request(child_path + '/snapshot')['sessionId'] == child
        child_before = fingerprint(child_history)
        before = fingerprint(main)
        # Empty progress is a synthetic, no-model sidecar when native has none.
        progress = Path(str(main) + '.progress.jsonl')
        if not progress.exists():
            progress.touch(exist_ok=False)
        progress_before = fingerprint(progress)
        old_native = observe_native(sid)
        request(path + '/metadata', {'archived': True})
        archived = entry(sid)
        assert archived['archived'] and archived['status'] == 'offline'
        assert any(value['sessionId'] == sid for value in request('/api/v1/sessions?archived=true')['sessions'])
        assert_exited(old_native)
        assert not native_pids(sid), 'archive left a matching native child'
        assert history(sid) == main and fingerprint(main) == before
        assert fingerprint(progress) == progress_before
        checks.update(archiveOffline=True, archiveNativeExited=True, archiveHistoryPreserved=True,
                      archiveProgressPreserved=True)
        expect_status(path + '/open', 409, b'{}')
        assert not native_pids(sid), 'rejected open spawned a native child'
        request(path + '/metadata', {'archived': False})
        restored = entry(sid)
        assert not restored['archived'] and restored['status'] == 'offline'
        assert not native_pids(sid), 'restore must not spawn'
        assert fingerprint(main) == before
        request(path + '/open', {})
        assert request(path + '/commands', {'type': 'get_state'})['success']
        new_native = observe_native(sid)
        assert new_native[0] != old_native[0], 'open must spawn a new native PID'
        assert history(sid) == main and fingerprint(main) == before
        checks.update(archivedOpenConflict=True, restoreNoSpawn=True, reopenNewNative=True,
                      reopenSameHistory=True)

        # Populate only native-known temp companions; no fabricated transcript entries.
        known = [main, progress, Path(str(main) + '.tmp'), Path(str(main) + '.progress.jsonl.tmp')]
        for companion in known[2:]:
            if not companion.exists():
                companion.touch(exist_ok=False)
        timings = registry.with_suffix('.tool-timings')
        for suffix in ['.json', '.json.tmp']:
            candidate = timings / (sid + suffix)
            if candidate.exists():
                known.append(candidate)
        if win:
            # Hold ONLY our synthetic tmp companion without FILE_SHARE_DELETE.
            # This proves retained deletion intent and retry after a real sharing violation.
            locked = known[3]
            handle = win.k.CreateFileW(str(locked), 0x80000000, 0x1 | 0x2, None, 3, 0x80, None)
            assert handle not in (None, ctypes.c_void_p(-1).value), 'fixture file lock failed'
            try:
                expect_status(path, 503, method='DELETE')
                assert_exited(new_native)
                assert locked.is_file(), 'locked fixture must remain until retry'
                pending = json.loads(registry.read_text(encoding='utf-8'))
                assert any(value['sessionId'] == sid for value in pending['sessions'])
                assert sid in pending.get('deletions', {}), 'failed delete lost retained manifest'
                expect_status(path + '/open', 409, b'{}')
                assert not native_pids(sid), 'pending deletion allowed a new native writer'
            finally:
                win.check(win.k.CloseHandle(handle))
            checks['lockedFileDeleteRetry'] = True
        request(path, method='DELETE')
        fixtures.remove(sid)
        assert_exited(new_native)
        assert not native_pids(sid)
        assert all(not item.exists() for item in known), 'delete retained known native history/sidecar'
        stored = json.loads(registry.read_text(encoding='utf-8'))
        assert all(value['sessionId'] != sid for value in stored['sessions'])
        assert all(sid not in stored.get(key, {}) for key in ['preferences', 'resources', 'deletions'])
        expect_status(path + '/open', 404, b'{}')
        assert not native_pids(sid) and all(not item.exists() for item in known), 'late open recreated deleted session'
        assert not list(history_root.glob('**/*_' + sid + '.jsonl'))
        assert request(child_path + '/commands', {'type': 'get_state'})['success']
        assert request(child_path + '/snapshot')['sessionId'] == child
        assert history(child) == child_history and fingerprint(child_history) == child_before
        checks.update(deleteNativeExited=True, deleteKnownFilesGone=True, deleteRegistryGone=True,
                      lateOpenNotFound=True, lateOpenNoRecreation=True, cloneIndependentHistory=True,
                      cloneSurvivesParentDelete=True)
        provenance['historyBeforeArchive'] = {'size': before[0], 'sha256': before[1]}
        provenance['knownDeletionFiles'] = len(known)
    finally:
        try:
            stop()
        finally:
            if win:
                for handle in native_handles:
                    win.k.CloseHandle(handle)
    assert cleanup[-1]['deleteFailures'] == 0, 'fixture DELETE cleanup failed'
    assert not json.loads(registry.read_text(encoding='utf-8'))['sessions'], 'cleanup retained registry fixtures'
    checks['cleanup'] = True
# Print success only after tree cleanup AND temporary-directory removal succeeded.
print(json.dumps({'passed': True, **checks, **provenance, 'syntheticOnly': True,
                  'noModelCalls': True, 'cleanupDetails': cleanup}))
