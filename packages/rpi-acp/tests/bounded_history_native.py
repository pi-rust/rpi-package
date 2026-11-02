"""Real-native, model-free regression for bounded active-branch ACP replay.

python -B tests/bounded_history_native.py --bridge <bridge> --rpi <rpi>
Use --diagnostics-dir to retain wire logs (and the fixture on failure).
The native v4 log is seeded only after the creating bridge has exited.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

from restart import fixture_environment


class Peer:
    """Owned stdio process with bounded waits and joined pipe readers."""

    def __init__(self, command, env, cwd, logs, name, native=False):
        self.native = native
        self.output = queue.Queue()
        self.next_id = 0
        self.log = (logs / (name + ".jsonl")).open("w", encoding="utf-8")
        self.stderr = (logs / (name + ".stderr.log")).open("w", encoding="utf-8")
        self.lock = threading.Lock()
        self.process = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        text=True, encoding="utf-8")
        self.readers = [threading.Thread(target=self.read_output),
                        threading.Thread(target=self.read_errors)]
        for reader in self.readers:
            reader.start()

    def record(self, direction, message):
        with self.lock:
            self.log.write(json.dumps(dict(direction=direction, message=message)) + "\n")
            self.log.flush()

    def read_output(self):
        try:
            for line in self.process.stdout:
                message = json.loads(line)
                self.record("out", message)
                self.output.put(message)
        except Exception as error:
            self.output.put(error)
        finally:
            self.output.put(EOFError("subprocess stdout closed"))

    def read_errors(self):
        for line in self.process.stderr:
            self.stderr.write(line)
            self.stderr.flush()

    def receive(self, deadline):
        try:
            message = self.output.get(timeout=max(.01, deadline - time.monotonic()))
        except queue.Empty:
            raise AssertionError("response timeout; inspect " + str(self.log.name)) from None
        if isinstance(message, Exception):
            raise message
        return message

    def ready(self):
        deadline = time.monotonic() + 25
        while True:
            message = self.receive(deadline)
            if message.get("type") == "ready":
                assert "get_messages" in message["supportedCommands"], message
                return

    def request(self, method, params):
        self.next_id += 1
        message = dict(id=self.next_id, **params, type=method) if self.native else dict(
            jsonrpc="2.0", id=self.next_id, method=method, params=params)
        self.record("in", message)
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        updates = []
        deadline = time.monotonic() + 40
        while True:
            response = self.receive(deadline)
            if response.get("id") == self.next_id:
                return response, updates
            updates.append(response)

    def close(self):
        """Prefer EOF; on a failed run terminate only the process we created."""
        try:
            self.process.stdin.close()
            try:
                code = self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=10)
                raise AssertionError("owned subprocess did not exit after EOF") from None
            assert code == 0, ("subprocess exit", code, self.stderr.name)
        finally:
            for reader in self.readers:
                reader.join(timeout=15)
            alive = [reader.name for reader in self.readers if reader.is_alive()]
            if not alive:
                self.process.stdout.close()
                self.process.stderr.close()
                self.log.close()
                self.stderr.close()
            assert not alive, ("subprocess output pipes still open", alive)


def success(peer, method, params):
    response, updates = peer.request(method, params)
    assert "error" not in response, response
    if peer.native:
        assert response.get("success") is True, response
        return response["data"], updates
    assert "result" in response, response
    return response["result"], updates


def seed(path):
    entries = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    assert entries[0]["version"] == 4, entries[0]
    assert not any(e.get("type") == "message" for e in entries), entries
    seq = max(e.get("seq", 0) for e in entries)
    leaf = None
    for entry in entries:
        if entry.get("kind") == "entry" and entry.get("lane") == "main":
            leaf = entry["id"]
        elif entry.get("kind") == "lane" and entry.get("lane") == "main":
            leaf = entry.get("leafId")
    expected = [f"ACP_LARGE_ACTIVE_{i:04d}" for i in range(513)]
    ids = []
    records = []

    def append(parent, text):
        nonlocal seq
        seq += 1
        entry_id = str(uuid.uuid4())
        records.append(dict(kind="entry", lane="main", type="message", id=entry_id,
                            seq=seq, parentId=parent, timestamp=int(time.time() * 1000),
                            message=dict(kind="user", role="user", content=text, timestamp=0)))
        return entry_id

    for text in expected:
        leaf = append(leaf, text)
        ids.append(leaf)
    active_leaf = leaf
    # A genuine sibling continuation at message 257. Physically last entries are
    # deliberately NOT active: only the native lane pointer selects the branch.
    off_leaf = ids[256]
    for i in range(19):
        off_leaf = append(off_leaf, f"ACP_LARGE_OFFBRANCH_{i:04d}")
    seq += 1
    records.append(dict(kind="lane", seq=seq, lane="main", leafId=active_leaf))
    with path.open("a", encoding="utf-8") as output:
        for record in records:
            output.write(json.dumps(record) + "\n")
    return expected, ids


def native_pages(rpi, env, root, logs, path, expected, ids):
    # Match the bridge's native launch convention. --session is an identifier
    # lookup (not a Windows absolute filename) in this host version.
    directory = path.parent.parent
    peer = Peer([rpi, "--mode", "rpc", "--session-id", directory.name,
                 "--session-dir", str(directory)],
                env, root, logs, "native-pages", native=True)
    try:
        peer.ready()
        pages = []
        before = None
        for _ in range(6):
            params = dict(limit=128)
            if before is not None:
                params["beforeId"] = before
            data, _ = success(peer, "get_messages", params)
            messages = data["messages"]
            assert data["total"] == 513 and data["leafId"] == ids[-1], data
            assert data["beforeId"] == before and 0 < len(messages) <= 128, data
            assert data["firstId"] == messages[0]["entryId"], data
            assert data["lastId"] == messages[-1]["entryId"], data
            pages.append(messages)
            if not data["hasMore"]:
                break
            assert len(messages) == 128, data
            before = data["firstId"]
        assert [len(page) for page in pages] == [128, 128, 128, 128, 1]
        ordered = [message for page in reversed(pages) for message in page]
        assert [m["entryId"] for m in ordered] == ids
        assert [m["content"] for m in ordered] == expected
    finally:
        peer.close()


def run(bridge, rpi, root, logs):
    from compact_protocol import controls, environment
    env = environment(root)
    store = root / "sessions"
    env.update(RPI_BIN=rpi, RPI_ACP_SESSION_DIR=str(store),
               RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1")
    first = Peer([bridge], env, root, logs, "create")
    try:
        result, _ = success(first, "initialize", dict(protocolVersion=1))
        assert result["agentCapabilities"]["loadSession"] is True
        result, _ = success(first, "session/new", dict(cwd=str(root), mcpServers=[]))
        session_id = result["sessionId"]
        uuid.UUID(session_id)
    finally:
        first.close()
    files = list((store / session_id).rglob("*.jsonl"))
    assert len(files) == 1, files
    expected, ids = seed(files[0])  # All processes/readers have stopped here.
    native_pages(rpi, env, root, logs, files[0], expected, ids)
    params = dict(sessionId=session_id, cwd=str(root), mcpServers=[])
    # Both loads must start a fresh bridge/native backend and replay everything
    # before their response. A following RPC fences any stray late updates.
    for name, method in (("load-first", "session/load"),
                         ("load-restart", "session/load"),
                         ("resume-restart", "session/resume")):
        peer = Peer([bridge], env, root, logs, name)
        try:
            success(peer, "initialize", dict(protocolVersion=1))
            result, updates = success(peer, method, params)
            assert "configOptions" in result, result
            if method == "session/load":
                assert len(updates) == 513, (name, len(updates))
                texts = []
                for item in updates:
                    assert item["method"] == "session/update", item
                    assert item["params"]["sessionId"] == session_id, item
                    update = item["params"]["update"]
                    assert update["sessionUpdate"] == "user_message_chunk", update
                    assert update["content"]["type"] == "text", update
                    texts.append(update["content"]["text"])
                assert texts == expected, (name, texts)
            else:
                assert not controls(updates, session_id)[0], updates
            _, updates = success(peer, "session/resume", params)
            assert not controls(updates, session_id)[0], ("late or repeated history", updates)
            for missing in ("rpi-old-1", str(uuid.uuid4())):
                response, updates = peer.request("session/load", dict(params, sessionId=missing))
                assert "error" in response and not controls(updates, session_id)[0], (response, updates)
            # All bytes after the last response are retained by the reader. Once
            # EOF has joined it, there must be no additional replay notifications.
        finally:
            peer.close()
        remaining = []
        while not peer.output.empty():
            item = peer.output.get_nowait()
            if not isinstance(item, EOFError):
                remaining.append(item)
        assert not controls(remaining, session_id)[0], ("updates after final response", remaining)
    print("PASS: native v4 fork; 513 active / 19 offbranch; five 128-bounded native pages; "
          "exact oldest-first replay before load response twice across restart; "
          "resume without replay; unknown sessions fail; graceful EOF and joined readers")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--rpi", required=True)
    parser.add_argument("--diagnostics-dir", type=Path)
    args = parser.parse_args()
    logs = args.diagnostics_dir or Path(tempfile.mkdtemp(prefix="large-history-logs-"))
    logs.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="fixture-", dir=logs)).resolve()
    try:
        run(str(Path(args.bridge).resolve()), str(Path(args.rpi).resolve()), root, logs)
    except BaseException:
        print("FAIL: preserving fixture and wire logs in", logs)
        raise
    else:
        shutil.rmtree(root)
        print("Wire logs:", logs)
