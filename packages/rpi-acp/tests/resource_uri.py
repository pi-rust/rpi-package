"""Real bridge + native + loopback-only model resource reference regression.

python -B tests/resource_uri.py --bridge <bridge> --rpi <rpi>
No user configuration or credentials; resources are never fetched by the test.
"""
import argparse
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import tempfile
import threading

from restart import Client, fixture_environment


def check(bridge, rpi):
    with tempfile.TemporaryDirectory(prefix="acp-resource-uri-") as directory:
        root = Path(directory)
        calls, fetches, errors = [], [], []

        class Handler(BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(10)

            def log_message(self, *_args):
                pass

            def do_GET(self):
                fetches.append(self.path)
                self.send_error(404)

            def do_POST(self):
                try:
                    assert self.path == "/v1/chat/completions", self.path
                    length = int(self.headers["Content-Length"])
                    assert 0 < length < 2**22
                    calls.append(json.loads(self.rfile.read(length)))
                    events = [
                        {"choices": [{"index": 0, "delta": {"role": "assistant", "content": "URI_FIXTURE_OK"}, "finish_reason": None}]},
                        {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                         "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}},
                    ]
                    data = ("".join("data: " + json.dumps(event) + "\n\n" for event in events)
                            + "data: [DONE]\n\n").encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except Exception as error:
                    errors.append(repr(error))
                    self.close_connection = True

        server = HTTPServer(("127.0.0.1", 0), Handler)
        server.timeout = 10
        server_thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=.05))
        server_thread.start()
        client = None
        try:
            env = dict(os.environ, **fixture_environment(root))
            agent = root / "agent"
            models_path = agent / "models.json"
            models = json.loads(models_path.read_text(encoding="utf-8"))
            models["providers"]["acp-fixture"]["baseUrl"] = f"http://127.0.0.1:{server.server_port}/v1"
            # The shared restart fixture's 8K context is smaller than native's
            # compaction reserve. Use a normal context so this is one prompt,
            # not an incidental auto-compaction call plus the actual prompt.
            models["providers"]["acp-fixture"]["models"][0]["contextWindow"] = 100000
            models_path.write_text(json.dumps(models), encoding="utf-8")
            (agent / "auth.json").write_text("{}", encoding="utf-8")
            env.update(RPI_BIN=rpi, RPI_ACP_SESSION_DIR=str(root / "sessions"),
                       RPI_ACP_NO_AUTO_REGISTER_ZED="1", RPI_ACP_NO_PERMISSION_GATE="1")
            workspace = root / "workspace"
            workspace.mkdir()
            first, second = workspace / "one" / "same", workspace / "two" / "same"
            first.mkdir(parents=True)
            second.mkdir(parents=True)
            canary = "DO_NOT_READ_DIRECTORY_CANARY_73c4b9"
            (first / "private.txt").write_text(canary, encoding="utf-8")
            original = (first / "private.txt").read_bytes()
            uris = [first.as_uri() + "/?view=one%2Ftwo#part", second.as_uri() + "/#other",
                    f"http://127.0.0.1:{server.server_port}/resource?q=one%20two#part",
                    "file:///C:/my%20folder/source/?revision=2#L4"]
            blocks = [{"type": "text", "text": "Check resource reference preservation only."}]
            blocks += [{"type": "resource_link", "name": "same", "uri": uri} for uri in uris[:2]]
            blocks += [{"type": "resource_link", "name": "", "uri": uris[2]},
                       {"type": "resource", "resource": {"uri": uris[3]}}]
            expected = ("Check resource reference preservation only.\n"
                        f"@same ({uris[0]})\n@same ({uris[1]})\n@{uris[2]}\n@{uris[3]}")
            client = Client([bridge], env)
            response, _ = client.request("initialize", {"protocolVersion": 1})
            assert "result" in response, response
            response, _ = client.request("session/new", {"cwd": str(workspace), "mcpServers": []})
            session = response["result"]["sessionId"]
            response, updates = client.request("session/prompt", {"sessionId": session, "prompt": blocks})
            assert response.get("result", {}).get("stopReason") == "end_turn", (response, client.errors)
            assert len(calls) == 1 and not errors and not fetches, (calls, errors, fetches)
            texts = [message["content"] for message in calls[0]["messages"] if message["role"] == "user"]
            # Providers may encode text as an OpenAI content-part array.
            def as_text(content):
                return content if isinstance(content, str) else "".join(part.get("text", "") for part in content)
            assert any(as_text(text) == expected for text in texts), texts
            assert canary not in json.dumps(calls) and (first / "private.txt").read_bytes() == original
            assert any("URI_FIXTURE_OK" in json.dumps(update) for update in updates), updates
            print("PASS: exact resource URIs reached the real native provider request; same labels distinguished; no resource HTTP fetch, directory canary exposure or file modification")
        finally:
            try:
                if client is not None:
                    client.close()
            finally:
                server.shutdown()
                server.server_close()
                server_thread.join(timeout=10)
                assert not server_thread.is_alive(), "loopback fixture thread did not stop"


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--rpi", required=True)
    args = parser.parse_args()
    check(args.bridge, args.rpi)
