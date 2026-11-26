#!/usr/bin/env python3
"""Validate the assembled blogs package against its original checkout."""

import argparse
from contextlib import contextmanager
from functools import partial
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
from threading import Thread
from zipfile import ZipFile

from package_command import PackageHost

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "packages" / "blogs"
ARTICLE = "content/blog/ddd-architecture.md"
RESULT = "博客审稿指令已读取写作方法、审稿规则和目标文章。"


def expected_tools():
    return set(json.loads((PACKAGE / "rpi-package.json").read_text())["agent"]["tools"])


def validate_archives(dist, blog):
    release = json.loads((dist / "release.json").read_text())
    lock = json.loads((PACKAGE / "extensions.lock.json").read_text())
    names = subprocess.check_output(["git", "ls-files", "-z", "--", "skills", "optional-skills"], cwd=blog)
    sources = {os.fsdecode(name): blog / os.fsdecode(name) for name in names.rstrip(b"\0").split(b"\0")}
    sources["instructions/SYSTEM.md"] = blog / ".rpi/SYSTEM.md"
    assert release["name"] == "blogs" and set(release["targets"]) == set(lock["targets"])
    for target, item in release["targets"].items():
        asset = dist / item["asset"]
        data = asset.read_bytes()
        assert len(data) == item["bytes"] and hashlib.sha256(data).hexdigest() == item["sha256"]
        with ZipFile(asset) as archive:
            assert archive.read("rpi-package.json") == (PACKAGE / "rpi-package.json").read_bytes()
            for path, source in sources.items():
                assert archive.read(path) == source.read_bytes(), path
            libraries = {item["targets"][target]["library"]["path"]: item["targets"][target]["library"]["sha256"]
                         for item in lock["extensions"]}
            assert {name.removeprefix("extensions/") for name in archive.namelist() if name.startswith("extensions/")} == set(libraries)
            for path, digest in libraries.items():
                assert hashlib.sha256(archive.read(f"extensions/{path}")).hexdigest() == digest, path
            assert not any(name.startswith(("content/", ".rpi/")) for name in archive.namelist())
    return len(sources) - 1


def fixture_handler(directory, requests, blog, agent_home):
    package = agent_home / "packages" / "blogs"
    resource_files = ["skills/write-router/SKILL.md", "skills/write-router/references/editorial-gate.md", ARTICLE]
    markers = ["通用写作方法", "审稿", "## 三层架构的问题"]
    tools = expected_tools()

    class Handler(SimpleHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            try:
                assert self.path == "/v1/chat/completions", self.path
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                messages = request["messages"]
                system = "\n".join(item["content"] for item in messages if item["role"] in {"system", "developer"})
                assert "博客写作系统提示词" in system and str(package) in system, system[:1000]
                assert "GLOBAL_SKILL_SENTINEL" not in system and "write-router" in system
                assert {tool["function"]["name"] for tool in request["tools"]} == tools
                assert any(item["role"] == "user" and f'"{ARTICLE}"' in item["content"] for item in messages)
                results = [item["content"] for item in messages if item["role"] == "tool"]
                for content, marker in zip(results, markers):
                    assert marker in content, content[:300]
                if len(results) < len(resource_files):
                    path = resource_files[len(results)]
                    path = path if path == ARTICLE else str(package / path)
                    delta = {"tool_calls": [{"index": 0, "id": f"fixture-read-{len(results)}", "type": "function",
                                             "function": {"name": "read", "arguments": json.dumps({"path": path})}}]}
                    finish = "tool_calls"
                else:
                    assert (blog / ARTICLE).read_text().splitlines()[1] in results[-1]
                    delta, finish = {"content": RESULT}, "stop"
                payload = json.dumps({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}, ensure_ascii=False)
                body = f"data: {payload}\n\ndata: [DONE]\n\n".encode()
                requests.append(self.path)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except (AssertionError, KeyError, TypeError) as error:
                self.send_error(400, repr(error))
                raise

    return partial(Handler, directory=directory)


@contextmanager
def serve(directory, requests, blog, home):
    with ThreadingHTTPServer(("127.0.0.1", 0), fixture_handler(directory, requests, blog, home)) as server:
        worker = Thread(target=server.serve_forever)
        worker.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}"
        finally:
            server.shutdown()
            worker.join()


def snapshot(blog):
    return (subprocess.check_output(["git", "status", "--porcelain=v1", "-uall"], cwd=blog),
            hashlib.sha256((blog / ARTICLE).read_bytes()).hexdigest())


def smoke(rpi, blog, dist):
    original = snapshot(blog)
    count = validate_archives(dist, blog)
    requests = []
    with TemporaryDirectory(prefix="rpi-blogs-") as temporary:
        home = Path(temporary).resolve() / "agent"
        global_skill = home / "skills/sentinel/SKILL.md"
        global_skill.parent.mkdir(parents=True)
        global_skill.write_text("---\nname: sentinel\ndescription: GLOBAL_SKILL_SENTINEL\n---\nfixture\n")
        env = dict(os.environ, RPI_CODING_AGENT_DIR=str(home), NO_PROXY="127.0.0.1",
                   RPI_EXTENSIONS_DIR=str(blog / ".rpi/extensions"))
        env.pop("RPI_OFFLINE", None)
        with serve(dist, requests, blog, home) as base_url:
            (home / "models.json").write_text(json.dumps({"providers": {"fixture": {
                "api": "openai-completions", "baseUrl": f"{base_url}/v1", "apiKey": "loopback-only",
                "models": [{"id": "blog", "contextWindow": 131072, "maxTokens": 512}],
            }}}))
            install = [rpi, "install", "package", "blogs", "--manifest", f"{base_url}/release.json"]
            first = subprocess.run(install, env=env, cwd=blog, capture_output=True, text=True, timeout=110)
            assert first.returncode == 0 and json.loads(first.stdout)["success"], (first.stdout, first.stderr)
            repeat = subprocess.check_output(install, env=env, cwd=blog, text=True, timeout=110)
            assert json.loads(repeat)["data"]["outcome"] == "unchanged", repeat
            options = ["--model", "fixture/blog", "--approve"]
            with PackageHost(rpi, "blogs", options, cwd=blog, env=env) as host:
                state = host.request("get_resource_state", version=1)["data"]
                assert state["package"]["name"] == "blogs", state
                assert set(state["modelEffectiveTools"]) == expected_tools(), state
                assert {item["name"] for item in state["loadedSkills"]} == {
                    "analytics", "archify", "blog-qa", "content-strategy", "image", "schema", "seo-audit",
                    "site-architecture", "social", "visualize", "write-router",
                }, state["loadedSkills"]
                names = {Path(item["path"]).stem.removeprefix("lib") for item in state["loadedExtensions"]}
                assert names == {"rpi_mcp_adapter", "rpi_voice", "rpi_webfetch", "rpi_websearch"}, names
                assert "voice" in {item["name"].lstrip("/") for item in host.request("get_commands")["data"]["commands"]}
                listing = host.request("list_package_commands")["data"]["commands"]
                assert [item["name"] for item in listing] == ["review"]
                invalid = host.request("invoke_package_command", command="review", args={})
                assert not invalid["success"] and "missing required argument path" in invalid["error"]
                assert not requests
                assert host.invoke("review", {"path": ARTICLE}) == RESULT
            with PackageHost(rpi, "blogs", [*options, "--continue"], cwd=blog, env=env) as host:
                restored = host.request("get_resource_state", version=1)["data"]
                assert restored["sessionId"] == state["sessionId"]
            assert len(requests) == 4, requests
    assert snapshot(blog) == original, "blogs working tree changed"
    print(f"PASS blogs: {count} original skill files, original prompt, 4 native extensions, review and resume")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpi", required=True, type=Path)
    parser.add_argument("--blogs", required=True, type=Path)
    parser.add_argument("--dist", default=ROOT / "dist", type=Path)
    args = parser.parse_args()
    smoke(str(args.rpi.resolve(strict=True)), args.blogs.resolve(strict=True), args.dist.resolve(strict=True))


if __name__ == "__main__":
    main()
