#!/usr/bin/env python3
"""Small stdlib-only Python 3.10 dashboard RPC probe.

Admin token resolution in CLI lib.rs::resolve_admin_auth_token is:
DUDUCLAW_AUTH_TOKEN, then <DuDuClaw home>/config.toml [gateway]
auth_token_enc (encrypted), then legacy auth_token (plaintext). There is no
first-run admin-token file. `duduclaw onboard --yes` creates configuration
non-interactively, but does NOT generate an admin token. For an isolated test
home, generate a random token with secrets.token_hex(32), add it as [gateway]
auth_token before startup, and save the same value in a mode-0600 token file.
Do not copy auth_token_enc into --token-file: it is ciphertext, not a token.
Existing MFA-required homes disallow legacy token login; use a fresh test home.

Default URL: DUDUCLAW_PORT > [gateway] port in config.toml > 18889, matching
core/config.rs. Home: DUDUCLAW_HOME or ~/.duduclaw. Python 3.10's fallback
reads a conventional [gateway] section; use --url for unusual TOML layouts.
Only plaintext ws:// is supported; use loopback or an SSH tunnel.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import struct
import sys
import time
from urllib.parse import urlsplit


class ProbeError(Exception):
    pass


def default_url():
    custom_home = os.environ.get("DUDUCLAW_HOME", "")
    home = Path(custom_home if custom_home.strip() else Path.home() / ".duduclaw")
    port = 18889
    try:
        raw = (home / "config.toml").read_text(encoding="utf-8")
    except FileNotFoundError:
        raw = ""
    try:
        import tomllib  # Python 3.11+, optional; no external dependency
    except ImportError:
        section = ""
        value = None
        for line in raw.splitlines():
            match = re.fullmatch(r"\s*\[([^\[\]]+)\]\s*(?:#.*)?", line)
            if match:
                section = match[1].strip()
            elif line.lstrip().startswith("["):
                section = ""
            elif section == "gateway":
                match = re.fullmatch(r"\s*port\s*=\s*([+\-]?[0-9][0-9a-fA-F_xob]*)\s*(?:#.*)?", line)
                if match:
                    number = match[1].replace("_", "")
                    try:
                        value = int(number, 0 if number.startswith(("0x", "0o", "0b")) else 10)
                    except ValueError:
                        value = None
    else:
        try:
            value = tomllib.loads(raw).get("gateway", {}).get("port")
        except (ValueError, AttributeError):
            value = None
    if type(value) is int and 0 <= value <= 65535:
        port = value
    env = os.environ.get("DUDUCLAW_PORT", "").strip()
    if re.fullmatch(r"\+?[0-9]+", env) and 0 <= int(env) <= 65535:
        port = int(env)
    return f"ws://127.0.0.1:{port}/ws"


class WebSocket:
    LIMIT = 8 * 1024 * 1024

    def __init__(self, url, timeout=20):
        self.sock = None
        self.buffer = bytearray()
        self.deadline = time.monotonic() + timeout
        parsed = urlsplit(url)
        if (parsed.scheme != "ws" or not parsed.hostname or parsed.username
                or parsed.password or parsed.fragment):
            raise ProbeError("expected ws://host:port/ws without credentials")
        port = parsed.port or 80
        self.sock = socket.create_connection((parsed.hostname, port), timeout=timeout)
        host = parsed.hostname
        if ":" in host:
            host = f"[{host}]"
        host = f"{host}:{port}"
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        if any(c in host + path for c in "\r\n"):
            raise ProbeError("invalid URL")
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        request = (f"GET {path} HTTP/1.1\r\nHost: {host}\r\n"
                   "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                   f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
        self.sock.sendall(request.encode("ascii"))
        while b"\r\n\r\n" not in self.buffer:
            self._more()
            if len(self.buffer) > 65536:
                raise ProbeError("oversized handshake")
        header, remaining = bytes(self.buffer).split(b"\r\n\r\n", 1)
        self.buffer = bytearray(remaining)
        lines = header.decode("latin1").split("\r\n")
        if len(lines[0].split()) < 2 or lines[0].split()[1] != "101":
            raise ProbeError("WebSocket handshake rejected")
        fields = {}
        for line in lines[1:]:
            name, sep, value = line.partition(":")
            if not sep:
                raise ProbeError("invalid handshake header")
            fields[name.lower()] = value.strip()
        expected = base64.b64encode(hashlib.sha1(
            (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")
        ).digest()).decode("ascii")
        if (fields.get("sec-websocket-accept") != expected
                or fields.get("upgrade", "").lower() != "websocket"
                or "upgrade" not in [v.strip() for v in fields.get("connection", "").lower().split(",")]):
            raise ProbeError("invalid WebSocket handshake")

    def _more(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise ProbeError("RPC timed out")
        self.sock.settimeout(remaining)
        data = self.sock.recv(65536)
        if not data:
            raise ProbeError("WebSocket closed")
        self.buffer.extend(data)

    def read(self, size):
        while len(self.buffer) < size:
            self._more()
        result = bytes(self.buffer[:size])
        del self.buffer[:size]
        return result

    def send(self, opcode, payload):
        mask = os.urandom(4)
        size = len(payload)
        header = bytes([0x80 | opcode])
        if size < 126:
            header += bytes([0x80 | size])
        elif size <= 65535:
            header += b"\xfe" + struct.pack("!H", size)
        else:
            header += b"\xff" + struct.pack("!Q", size)
        masked = bytes(v ^ mask[i % 4] for i, v in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def message(self):
        parts = bytearray()
        started = False
        while True:
            first, second = self.read(2)
            final, opcode = bool(first & 0x80), first & 15
            if first & 0x70 or second & 0x80:
                raise ProbeError("invalid server frame")
            size = second & 127
            if size == 126:
                size = struct.unpack("!H", self.read(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.read(8))[0]
            if size > self.LIMIT or len(parts) + size > self.LIMIT:
                raise ProbeError("oversized server message")
            if opcode >= 8 and (not final or size > 125):
                raise ProbeError("invalid control frame")
            payload = self.read(size)
            if opcode == 8:
                raise ProbeError("WebSocket closed by gateway")
            if opcode == 9:
                self.send(10, payload)
                continue
            if opcode == 10:
                continue
            if opcode == 1 and not started:
                started = True
            elif opcode != 0 or not started:
                raise ProbeError("unexpected server frame")
            parts.extend(payload)
            if final:
                return json.loads(parts.decode("utf-8"))

    def call(self, request_id, method, params):
        self.deadline = time.monotonic() + 30
        self.send(1, json.dumps({"type": "req", "id": request_id,
                                 "method": method, "params": params}).encode("utf-8"))
        while True:
            frame = self.message()
            if not isinstance(frame, dict):
                raise ProbeError("invalid RPC response")
            if frame.get("type") == "res" and frame.get("id") == request_id:
                if frame.get("ok") is not True:
                    # Redacted by main, including if a server echoes the token.
                    raise ProbeError("RPC refused: " + json.dumps(frame.get("error"), ensure_ascii=False))
                return frame.get("payload")

    def close(self):
        if self.sock:
            self.sock.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url")
    parser.add_argument("--token-file", required=True, type=Path)
    parser.add_argument("--method", required=True)
    parser.add_argument("--params", default="{}")
    args = parser.parse_args()
    token = ""
    ws = None
    try:
        token = args.token_file.read_text(encoding="utf-8").strip()
        if not token or len(token) > 65536:
            raise ProbeError("empty or oversized token file")
        params = json.loads(args.params)
        if not isinstance(params, dict):
            raise ProbeError("params must be a JSON object")
        ws = WebSocket(args.url or default_url())
        ws.call("auth", "connect", {"token": token})
        result = ws.call("probe", args.method, params)
        # Never disclose the supplied credential, even on a token-returning RPC.
        output = json.dumps(result, ensure_ascii=True)
        encoded_token = json.dumps(token, ensure_ascii=True)[1:-1]
        print(output.replace(encoded_token, "[REDACTED]"))
        return 0
    except (OSError, ValueError, ProbeError, OverflowError) as exc:
        message = str(exc).replace(token, "[REDACTED]") if token else str(exc)
        if token:
            message = message.replace(json.dumps(token, ensure_ascii=False)[1:-1], "[REDACTED]")
        print("rpc_probe: " + message[:500].replace("\n", " "), file=sys.stderr)
        return 1
    finally:
        if ws:
            ws.close()


if __name__ == "__main__":
    sys.exit(main())
