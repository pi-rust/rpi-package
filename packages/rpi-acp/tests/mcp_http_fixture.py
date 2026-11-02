"""Loopback-only MCP Streamable HTTP fixture; no bridge/native imports.

Socket/worker timeouts and local owner cleanup are not a hard global deadline.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import socket
import threading
import time
import uuid
from urllib.parse import urlsplit

VERSION = "2024-11-05"
META = {"fixture": "HTTP_METADATA_83ac", "nested": {"n": 7}}


class Fixture:
    """Owns listener, sockets, non-daemon workers and blocked-call release events.

    DELETE is observed, not relied on for cleanup. close() locally releases every
    blocked worker and closes accepted sockets even when the client disappears.
    Fake Authorization/query values intentionally appear in fixture wire evidence
    and inbound ACP configuration. Tool definitions/results never echo them; this
    tests automatic configuration non-disclosure, not remote-content DLP.
    """
    def __init__(self, root, harness):
        self.root, self.h = root, harness
        self.lock = threading.RLock()
        self.events, self.errors, self.sessions = [], [], {}
        self.sockets, self.workers = set(), set()
        self.worker_ready = {}
        self.stopping = threading.Event()
        self.closed = False
        self.responses = []
        owner = self

        class Server(ThreadingHTTPServer):
            daemon_threads = False
            block_on_close = True

            def get_request(self):
                sock, addr = super().get_request()
                sock.settimeout(8)
                with owner.lock:
                    owner.sockets.add(sock)
                    owner.worker_ready[sock] = threading.Event()
                return sock, addr

            def process_request(self, request, address):
                # shutdown() must not finish while a newly started worker has
                # yet to register its positively-live socket audit.
                ready = owner.worker_ready[request]
                super().process_request(request, address)
                assert ready.wait(5), "HTTP worker did not register"
                with owner.lock:
                    owner.worker_ready.pop(request)

            def process_request_thread(self, request, address):
                thread = threading.current_thread()
                identity = uuid.uuid4().hex
                with owner.lock:
                    owner.workers.add(thread)
                    owner.h.append(root / "http-lifecycle.jsonl", dict(worker=identity,
                        live=thread.is_alive(), socketLive=request.fileno() >= 0))
                    owner.worker_ready[request].set()
                try:
                    super().process_request_thread(request, address)
                finally:
                    with owner.lock:
                        owner.sockets.discard(request)
                        owner.workers.discard(thread)
                        owner.h.append(root / "http-lifecycle.jsonl", dict(worker=identity,
                            finished=True, socketClosed=request.fileno() == -1))

            def handle_error(self, *_args):
                # Handler catches protocol failures; unexpected server failures
                # are retained rather than printed with fake wire secrets.
                with owner.lock:
                    owner.errors.append("unexpected HTTP worker failure")

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def reply(self, status, value=None, session=None, sse=False, location=None):
                data = b"" if value is None else json.dumps(value, ensure_ascii=False).encode()
                if sse:
                    # CRLF and multiline data, including split Unicode bytes.
                    encoded = json.dumps(value, ensure_ascii=False, indent=2)
                    data = (": fixture\r\nevent: message\r\n" + "".join(
                        "data: " + line + "\r\n" for line in encoded.splitlines()) + "\r\n").encode()
                audit = dict(response=uuid.uuid4().hex, method=getattr(self, "mcp_method", self.command),
                    mode=getattr(self, "mcp_mode", None), status=status, expectedBytes=len(data),
                    writtenBytes=0, writes=0, complete=False, dropped=False)
                try:
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(data)))
                    self.send_header("Content-Type", "text/event-stream" if sse else "application/json")
                    self.send_header("Connection", "close")
                    if session:
                        self.send_header("Mcp-Session-Id", session)
                    if location:
                        self.send_header("Location", location)
                    self.end_headers()
                    snow = data.find("雪".encode())
                    cuts = sorted(set([1, 9, 23, 41, len(data) // 2,
                        snow + 1 if snow >= 0 else 53, len(data)])) if sse else [len(data)]
                    start = 0
                    for end in cuts:
                        assert start <= end <= len(data), "invalid fixture response cut"
                        chunk = data[start:end]
                        written = self.wfile.write(chunk)
                        assert written == len(chunk), "short fixture write"
                        self.wfile.flush()
                        audit["writtenBytes"] += written
                        audit["writes"] += 1
                        if sse:
                            time.sleep(.003)
                        start = end
                    audit["complete"] = audit["writtenBytes"] == len(data)
                except OSError as error:
                    if not isinstance(error, (BrokenPipeError, ConnectionResetError, ConnectionAbortedError)) and not owner.stopping.is_set():
                        raise
                    audit.update(dropped=True, dropType=type(error).__name__)
                    if audit["mode"] not in ("blocked", "oversize") and not owner.stopping.is_set():
                        with owner.lock:
                            owner.errors.append("unexpected response drop")
                finally:
                    self.close_connection = True
                    audit["closeRequested"] = True
                    with owner.lock:
                        owner.responses.append(audit)
                        owner.h.append(root / "http-response-audit.jsonl", audit)

            def do_GET(self):
                owner.errors.append("unexpected GET (legacy SSE is not supported)")
                self.reply(405)

            def do_DELETE(self):
                self.handle_mcp(delete=True)

            def do_POST(self):
                self.handle_mcp(delete=False)

            def handle_mcp(self, delete):
                try:
                    size = int(self.headers.get("Content-Length", "0"))
                    assert 0 <= size < 1024 * 1024, "request size"
                    value = json.loads(self.rfile.read(size)) if size else {}
                    method = "DELETE" if delete else value.get("method")
                    self.mcp_method = method
                    # The core's parameterless notification serializes params as
                    # null; both absent and null mean no notification arguments.
                    params = value.get("params") or {}
                    self.mcp_mode = params.get("arguments", {}).get("mode")
                    path = urlsplit(self.path).path
                    auth = self.headers.get("Authorization")
                    session = self.headers.get("Mcp-Session-Id")
                    version = self.headers.get("MCP-Protocol-Version")
                    event = dict(method=method, path=self.path, auth=auth,
                                 session=session, version=version, request=value)
                    with owner.lock:
                        owner.events.append(event)
                        owner.h.append(root / "http-fixture-wire.jsonl", event)
                    assert auth in owner.tokens.values(), "unknown fixture authorization"
                    if path in ("/401", "/404", "/redirect"):
                        self.reply({"/401": 401, "/404": 404, "/redirect": 307}[path],
                                   location=owner.url("/redirect-target"))
                        return
                    assert path == "/mcp", "unexpected endpoint / redirect followed"
                    if method == "initialize":
                        assert not session, "initialize reused session"
                        assert value["params"]["protocolVersion"], "missing offered protocol"
                        session = "http-fixture-" + uuid.uuid4().hex
                        with owner.lock:
                            owner.sessions[session] = dict(auth=auth, initialized=False,
                                deleted=False, blocked={}, calls=[], pages=[])
                        self.reply(200, dict(jsonrpc="2.0", id=value["id"], result=dict(
                            protocolVersion=VERSION, capabilities={"tools": {}},
                            serverInfo=dict(name="native-http-fixture", version="1"))), session)
                        return
                    with owner.lock:
                        state = owner.sessions[session]
                        assert state["auth"] == auth, "cross-session credential"
                        assert version == VERSION, "missing/incorrect negotiated protocol header"
                        assert not state["deleted"], "request after DELETE"
                    if delete:
                        with owner.lock:
                            state["deleted"] = True
                        self.reply(200)
                        return
                    if method == "notifications/initialized":
                        with owner.lock:
                            assert not state["initialized"], "duplicate initialized"
                            state["initialized"] = True
                        self.reply(202)
                        return
                    assert state["initialized"], "request before initialized"
                    if method == "notifications/cancelled":
                        request_id = value["params"]["requestId"]
                        with owner.lock:
                            assert request_id in state["blocked"], "cancel did not correlate blocked request"
                            release = state["blocked"][request_id]
                        release.set()
                        self.reply(202)
                        return
                    if method == "tools/list":
                        cursor = value.get("params", {}).get("cursor")
                        state["pages"].append(cursor)
                        assert state["pages"] in ([None], [None, "page-two"]), "discovery retry/extra page"
                        result = dict(tools=[], nextCursor="page-two") if cursor is None else dict(tools=[
                            dict(name="probe", description="NATIVE_MCP_PROBE_LABEL", inputSchema=owner.h.SCHEMA)])
                    elif method == "tools/call":
                        params = value["params"]
                        assert params["name"] == "probe", "wrong tool"
                        args = params["arguments"]
                        assert args == dict(mode=args["mode"], query=owner.h.QUERY), "arguments changed"
                        mode = args["mode"]
                        with owner.lock:
                            state["calls"].append(args)
                        if mode == "blocked":
                            release = threading.Event()
                            with owner.lock:
                                state["blocked"][value["id"]] = release
                                # Readers synchronize on existence: publish atomically.
                                pending = root / "http-blocked.pending"
                                owner.h.dump(pending, dict(session=session, id=value["id"]))
                                pending.replace(root / "http-blocked.json")
                            deadline = time.monotonic() + 60
                            while not release.wait(.05) and not owner.stopping.is_set():
                                assert time.monotonic() < deadline, "blocked fixture lifetime exceeded"
                            # Deliberately late response after cancellation: must
                            # never become a second result or trigger replay.
                            result = dict(content=[dict(type="text", text="LATE_HTTP_RESULT")])
                        else:
                            label = next(k for k, v in owner.tokens.items() if v == auth)
                            result = dict(content=[dict(type="text", text=owner.h.TEXT + ":" + label,
                                annotations={"audience": ["assistant"], "priority": .5}, _meta=META)],
                                structuredContent=owner.h.STRUCTURED, isError=False, _meta=META)
                            if mode == "image":
                                result["content"].append(dict(type="image", data=owner.h.PNG, mimeType="image/png", _meta=META))
                            elif mode == "is_error":
                                result.update(content=[dict(type="text", text="MCP_EXPLICIT_ERROR")], isError=True)
                            elif mode == "unsupported":
                                result["content"] = [dict(type="resource_link", uri="urn:unsupported", name="x")]
                            elif mode == "oversize":
                                result["content"] = [dict(type="text", text="x" * (1100 * 1024))]
                            elif mode == "malformed_base64":
                                result["content"] = [dict(type="image", data="not!base64", mimeType="image/png")]
                            elif mode == "textSignature":
                                result["content"] = [dict(type="text", text=owner.h.TEXT, textSignature={"invalid": True})]
                            else:
                                assert mode == "success", "unknown mode"
                    else:
                        raise AssertionError("unexpected MCP method")
                    self.reply(200, dict(jsonrpc="2.0", id=value["id"], result=result),
                               sse=(method == "tools/call" and mode == "image") or
                                   (method == "tools/list" and cursor == "page-two"))
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    with owner.lock:
                        owner.h.append(root / "http-response-audit.jsonl", dict(requestDropped=True,
                            method=getattr(self, "mcp_method", None), closeRequested=True))
                    self.close_connection = True
                except BaseException as error:
                    with owner.lock:
                        # Never include exception values (which can contain wire secrets).
                        owner.errors.append(type(error).__name__)
                        owner.h.dump(root / "http-fixture-errors.json", owner.errors)
                    self.close_connection = True

        self.tokens = {label: "Bearer fakecanary-http-" + label for label in
                       ("one", "two", "load", "resume", "failure")}
        self.query_canary = "fakecanary-http-query-83ac"
        self.server = Server(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.03),
                                       name="http-mcp-listener", daemon=False)
        self.thread.start()
        assert self.thread.is_alive() and self.server.fileno() >= 0
        harness.append(root / "http-lifecycle.jsonl", dict(listenerLive=True, port=self.server.server_port))

    def secret_canaries(self):
        """Full static Authorization/query forms and stripped credential values."""
        return tuple(dict.fromkeys(["canary=" + self.query_canary, self.query_canary] +
            list(self.tokens.values()) +
            [value.removeprefix("Bearer ") for value in self.tokens.values()]))

    def url(self, path="/mcp"):
        return f"http://127.0.0.1:{self.server.server_port}{path}?canary={self.query_canary}"

    def config(self, label, path="/mcp"):
        return [dict(type="http", name=label, url=self.url(path),
                     headers=[dict(name="Authorization", value=self.tokens[label])])]

    def count(self, method=None):
        with self.lock:
            return sum(method is None or e["method"] == method for e in self.events)

    def session_for(self, label):
        with self.lock:
            matches = [(sid, s) for sid, s in self.sessions.items() if s["auth"] == self.tokens[label]]
            assert len(matches) == 1, "expected exactly one provisioned session for " + label
            sid, state = matches[0]
            assert state["initialized"] and state["pages"] == [None, "page-two"]
            return sid, state

    def close(self):
        if self.closed:
            return
        self.stopping.set()
        # Stop accepting BEFORE snapshotting sockets: get_request may otherwise
        # add a socket after our snapshot and leave a non-daemon worker behind.
        # shutdown()/server_close() synchronously wait. The local release/socket
        # timeouts below are ownership cleanup, NOT a whole-run hard watchdog.
        self.server.shutdown()
        with self.lock:
            for state in self.sessions.values():
                for release in state["blocked"].values():
                    release.set()
            sockets = list(self.sockets)
            workers = list(self.workers)
        for sock in sockets:
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            sock.close()
        self.server.server_close()  # joins non-daemon request workers
        self.thread.join(timeout=10)
        for worker in workers:
            worker.join(timeout=10)
        assert not self.thread.is_alive() and not any(w.is_alive() for w in workers), "HTTP workers leaked"
        assert self.server.fileno() == -1 and not self.sockets and not self.workers, "HTTP sockets leaked"
        lifecycle = [json.loads(line) for line in (self.root / "http-lifecycle.jsonl").read_text(encoding="utf-8").splitlines()]
        live = [e for e in lifecycle if "worker" in e and "live" in e]
        finished = [e for e in lifecycle if e.get("finished")]
        assert all(e["live"] and e["socketLive"] for e in live), "worker not positively live"
        assert sorted(e["worker"] for e in live) == sorted(e["worker"] for e in finished), "worker closure audit mismatch"
        assert all(e["socketClosed"] for e in finished), "accepted socket not closed"
        assert all(e["complete"] or e["dropped"] for e in self.responses), "response neither written nor dropped"
        assert all(e["closeRequested"] for e in self.responses), "response close missing"
        self.h.append(self.root / "http-lifecycle.jsonl", dict(listenerClosed=True, workersJoined=True,
            sessions=len(self.sessions), deletes=sum(s["deleted"] for s in self.sessions.values()),
            responses=len(self.responses), drops=sum(e["dropped"] for e in self.responses)))
        self.closed = True
        assert not self.errors, "fixture protocol errors; inspect http-fixture-errors.json"
