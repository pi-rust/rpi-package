"""Dedicated request-auth wire tests. No native credentials or installed binaries used."""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time

REV = "1142c83bbfa9407f9f264d1e290ffafcd54eb79e"
AGENT = "rpi-agent-credentials"

class Client:
    def __init__(self, bridge, fixture, mode):
        env = dict(os.environ, RPI_BIN=str(fixture), ACP_AUTH_FIXTURE=mode)
        self.p = subprocess.Popen([str(bridge)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        self.q = queue.Queue()
        def read():
            for line in self.p.stdout:
                self.q.put(json.loads(line))
        threading.Thread(target=read, daemon=True).start()
    def send(self, method, id, params):
        value = {"jsonrpc":"2.0","method":method,"params":params}
        if id is not None: value["id"] = id
        self.p.stdin.write(json.dumps(value)+"\n"); self.p.stdin.flush()
    def recv(self): return self.q.get(timeout=10)
    def reply(self, id, value):
        self.p.stdin.write(json.dumps({"jsonrpc":"2.0","id":id,"result":value})+"\n"); self.p.stdin.flush()
    def init(self, v, modes, opt=False):
        caps = {"elicitation":{m:{} for m in modes}}
        if opt: caps["_meta"] = {"rpi/unstable-session-rfds":REV}
        params = {"protocolVersion":v}
        params["capabilities" if v == 2 else "clientCapabilities"] = caps
        if v == 2: params["info"] = {"name":"test","version":"1"}
        self.send("initialize",1,params)
        value = self.recv(); assert value["id"] == 1, value
        return value["result"]
    def close(self):
        self.p.stdin.close(); self.p.wait(timeout=10)
        assert self.p.returncode == 0, self.p.stderr.read()
        assert "secret-fixture" not in self.p.stderr.read()


def main():
    p = argparse.ArgumentParser(); p.add_argument("--bridge",type=Path,required=True); p.add_argument("--fixture",type=Path,required=True); p.add_argument("--schemas",type=Path); a = p.parse_args()
    fixtures = []
    def validate(version, definition, value):
        fixtures.append((version,definition,value))
        if a.schemas:
            from jsonschema import Draft202012Validator
            schema = json.loads((a.schemas / f"schema-v{version}.json").read_text(encoding="utf-8"))
            if definition in schema["$defs"]:
                Draft202012Validator({"$defs":schema["$defs"],"$ref":f"#/$defs/{definition}"}).validate(value)
    for v in [1,2]:
        for mode, modes, supported in [("key",["form"],True),("oauth",["form","url"],True),("oauth",["url"],False),("oauth",["form"],False),("key",[],False),("unsupported",["form","url"],False),("bad-descriptor",["form"],False)]:
            c = Client(a.bridge,a.fixture,mode); result = c.init(v,modes,True)
            validate(v,"InitializeResponse",result)
            methods = result["authMethods"]
            assert bool(methods) == supported, result
            if supported:
                assert methods[0]["type"] == "agent" and methods[0]["methodId" if v == 2 else "id"] == AGENT
            c.send("auth/status",2,{})
            assert c.recv() == {"jsonrpc":"2.0","id":2,"result":{"authenticated":True}}
            c.send("auth/get_state",3,{})
            assert c.recv()["error"]["code"] == -32601
            c.send("auth/login" if v == 2 else "authenticate",4,{"methodId":"wrong"})
            assert c.recv()["error"]["code"] == -32602
            c.close()
        for mode, action in [("key","accept"),("oauth","accept"),("key","decline"),("oauth","cancel"),("key","malformed"),("bad-exit","accept"),("failed","accept")]:
            c = Client(a.bridge,a.fixture,mode); c.init(v,["form","url"])
            c.send("auth/status",2,{})
            assert c.recv()["error"]["code"] == -32601
            parent = "auth-parent"
            c.send("auth/login" if v == 2 else "authenticate",parent,{"methodId":AGENT})
            completions = 0; prompts = 0
            while True:
                frame = c.recv()
                if frame.get("method") == "elicitation/create":
                    prompts += 1; params = frame["params"]
                    assert params["requestId"] == parent and "sessionId" not in params and "toolCallId" not in params
                    validate(v,"ElicitationRequest",params)
                    answer = {"action":action,"content":{"answer":"secret-fixture"}}
                    if action == "malformed": answer = {"action":"accept","content":"secret-fixture"}
                    c.reply(frame["id"],answer)
                    if params["mode"] == "url":
                        # Browser consent is never immediate authentication success.
                        assert action != "accept" or mode == "oauth"
                elif frame.get("method") == "elicitation/complete":
                    completions += 1; assert frame["params"]["requestId"] == parent
                    validate(v,"ElicitationCompleteNotification",frame["params"])
                else:
                    assert frame["id"] == parent, frame
                    success = action == "accept" and mode in ("key","oauth")
                    assert ("result" in frame) == success, frame
                    assert "secret-fixture" not in json.dumps(frame)
                    assert completions == int(success and mode == "oauth")
                    assert prompts == (2 if success and mode == "oauth" else 1)
                    break
            c.close()
        # Cancellation and disconnect while an interaction is pending.
        for mode in ["key", "oauth"]:
            for disconnect in [False, True]:
                c = Client(a.bridge,a.fixture,mode); c.init(v,["form", "url"])
                c.send("auth/login" if v == 2 else "authenticate",42,{"methodId":AGENT})
                pending = c.recv()
                assert pending["method"] == "elicitation/create", pending
                if not disconnect:
                    c.send("$/cancel_request",None,{"requestId":42})
                    cancellation = None
                    while True:
                        frame = c.recv()
                        assert frame.get("method") != "elicitation/complete", frame
                        if frame.get("method") == "$/cancel_request":
                            assert cancellation is None, frame
                            assert frame["params"] == {"requestId":pending["id"]}, frame
                            validate(v,"CancelRequestNotification",frame["params"])
                            cancellation = frame
                        else:
                            assert frame["id"] == 42 and frame["error"]["code"] == -32800, frame
                            assert cancellation is not None, "parent completed before elicitation cancellation"
                            break
                c.close()
                assert c.q.empty(), "unexpected completion after cancellation/disconnect"
    print(f"PASS ACP auth: both protocol versions, capability gates, key/OAuth/code, typed errors, decline/cancel, parent cancellation/disconnect; {len(fixtures)} schema candidates")

if __name__ == "__main__": main()
