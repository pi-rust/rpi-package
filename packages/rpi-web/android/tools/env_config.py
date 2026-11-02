#!/usr/bin/env python3
"""Load only endpoint settings from ignored .env.local, without shell evaluation or logging values."""
from __future__ import annotations

import argparse
import ipaddress
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
DEFAULT = "RPI_WEB_ANDROID_DEFAULT_ORIGIN"
DEVELOPMENT = "RPI_WEB_ANDROID_DEVELOPMENT_ORIGIN"
ALLOWED = {DEFAULT, DEVELOPMENT}


def parse_dotenv(text: str) -> dict[str, str]:
    values = {}
    for number, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, separator, value = line.partition("=")
        key, value = key.strip(), value.strip()
        if not separator or key not in ALLOWED or key in values:
            raise ValueError(f"Invalid or duplicate endpoint setting in .env.local line {number}")
        if value.startswith(('"', "'")):
            if len(value) < 2 or value[-1] != value[0]:
                raise ValueError(f"Unclosed .env.local value on line {number}")
            value = value[1:-1]
        if any(char in value for char in ("$", "`", "\\", '"', "'")):
            raise ValueError(f"Shell expressions/escapes are not supported in .env.local line {number}")
        values[key] = value
    return values


def normalize_origin(raw: str, name: str, required: bool = False) -> str:
    raw = raw.strip()
    if not raw:
        if required:
            raise ValueError(name + " is required; set it in the environment or ignored .env.local")
        return ""
    try:
        uri = urlsplit(raw)
        host = uri.hostname
        port = uri.port
        if (not raw.isascii() or any(ord(c) <= 32 or c == "\\" for c in raw) or
                uri.scheme.lower() != "https" or not host or
                uri.username is not None or uri.password is not None or
                host.endswith(".") or "%" in host or uri.netloc.endswith(":") or
                uri.path not in ("", "/") or "?" in raw or "#" in raw or
                (port is not None and not 1 <= port <= 65535)):
            raise ValueError()
        # urlsplit alone accepts and discards suffixes after a bracketed IP.
        # Validate the ENTIRE authority before normalizing it for the child build.
        match = re.fullmatch(r"(\[[a-fA-F0-9:.]+\]|[a-zA-Z0-9.\-]+)(?::[0-9]+)?", uri.netloc)
        authority = "[" + host.lower() + "]" if ":" in host else host.lower()
        if not match or match[1].lower() != authority:
            raise ValueError()
        if ":" in host:
            ipaddress.IPv6Address(host)
        else:
            labels = host.split(".")
            if not all(re.fullmatch(r"[a-zA-Z0-9](?:[a-zA-Z0-9\-]*[a-zA-Z0-9])?", label) for label in labels):
                raise ValueError()
            if len(labels) > 1 and not labels[-1][0].isalpha():
                ipaddress.IPv4Address(host)
        return "https://" + authority + ("" if port in (None, 443) else f":{port}")
    except ValueError:
        # Do not echo invalid input: a mistaken URL may contain credentials.
        raise ValueError(name + " must be an HTTPS origin without credentials, path, query or fragment") from None


def load_environment(root: Path = ROOT, environ: dict[str, str] | None = None) -> dict[str, str]:
    environment = dict(os.environ if environ is None else environ)
    file = root / ".env.local"
    if file.is_file():
        for key, value in parse_dotenv(file.read_text(encoding="utf-8-sig")).items():
            environment.setdefault(key, value)
    environment[DEFAULT] = normalize_origin(environment.get(DEFAULT, ""), DEFAULT, required=True)
    environment[DEVELOPMENT] = normalize_origin(environment.get(DEVELOPMENT, ""), DEVELOPMENT)
    return environment


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs=argparse.REMAINDER, help="Command to run with endpoint variables; values are never printed")
    args = parser.parse_args()
    if not args.command:
        parser.error("a command is required")
    try:
        return subprocess.run(args.command, cwd=ROOT, env=load_environment()).returncode
    except (ValueError, OSError):
        print("Endpoint environment invalid; check the two documented origin variables and .env.local", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
