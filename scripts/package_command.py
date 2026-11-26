#!/usr/bin/env python3
"""Small terminal client for native package commands."""

import argparse
import json
import queue
import subprocess
import threading
from pathlib import Path


class PackageHost:
    def __init__(self, rpi, name, options=(), *, cwd=None, env=None):
        self.process = subprocess.Popen(
            [rpi, "run", "package", name, *options], cwd=cwd, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8",
        )
        self.frames = queue.Queue(maxsize=256)
        self.sequence = 0
        self.closed = threading.Event()
        self.reader = threading.Thread(target=self._read, daemon=True)

    def _read(self):
        try:
            for line in self.process.stdout:
                if self.closed.is_set():
                    return
                self.frames.put(json.loads(line), timeout=5)
        except (ValueError, OSError, queue.Full) as error:
            try:
                self.frames.put(error, timeout=5)
            except queue.Full:
                return
        finally:
            try:
                self.frames.put(None, timeout=5)
            except queue.Full:
                pass

    def receive(self, predicate):
        while True:
            try:
                frame = self.frames.get(timeout=45)
            except queue.Empty as error:
                raise RuntimeError("timed out waiting for native RPC") from error
            if frame is None:
                raise RuntimeError(f"native RPC closed (status {self.process.poll()})")
            if isinstance(frame, Exception):
                raise RuntimeError(f"invalid native RPC response: {frame}")
            if predicate(frame):
                return frame

    def request(self, kind, **fields):
        self.sequence += 1
        body = {"type": kind, "id": self.sequence, **fields}
        self.process.stdin.write(json.dumps(body, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        return self.receive(lambda frame: frame.get("type") == "response" and frame.get("id") == self.sequence)

    def invoke(self, command, args):
        response = self.request("invoke_package_command", command=command, args=args)
        if not response.get("success"):
            raise RuntimeError(response.get("error"))
        settled = self.receive(lambda frame: frame.get("type") == "agent_settled" and frame.get("requestId") == response["id"])
        if settled["outcome"] != "completed":
            raise RuntimeError(settled.get("error") or settled["outcome"])
        return settled["finalText"]

    def __enter__(self):
        self.reader.start()
        try:
            self.ready = self.receive(lambda frame: frame.get("type") == "ready")
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def __exit__(self, *_error):
        self.closed.set()
        try:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        finally:
            self.reader.join(timeout=6)
            self.process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpi", required=True, type=lambda value: str(Path(value).resolve()))
    parser.add_argument("package")
    parser.add_argument("command", nargs="?", help="omit to list all available commands")
    parser.add_argument("--args", type=json.loads, default={})
    parser.add_argument("--model")
    parser.add_argument("--approve", action="store_true")
    args = parser.parse_args()
    options = (["--model", args.model] if args.model else []) + (["--approve"] if args.approve else [])
    if not isinstance(args.args, dict):
        parser.error("--args must contain an object")
    try:
        with PackageHost(args.rpi, args.package, options) as host:
            if args.command:
                print(host.invoke(args.command, args.args))
            else:
                response = host.request("list_package_commands")
                if not response["success"]:
                    raise RuntimeError(response["error"])
                print(json.dumps(response["data"], ensure_ascii=False, indent=2))
    except (RuntimeError, OSError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()
