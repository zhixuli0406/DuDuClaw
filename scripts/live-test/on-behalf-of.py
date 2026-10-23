#!/usr/bin/env python3
"""Probe a disposable home's real MCP registration, without calling a model.

Usage: on-behalf-of.py HOME BINARY
Create HOME with make-home.sh and boot BINARY there once first. This probe
writes tasks, queue rows and configuration; it refuses the production home.
The gateway must be stopped before running it.
"""

import json
import os
from pathlib import Path
import queue
import re
import sqlite3
import subprocess
import sys
import threading


def main():
    home = Path(sys.argv[1]).resolve()
    binary = Path(sys.argv[2]).resolve()
    production = (Path.home() / ".duduclaw").resolve()
    if home == production or production in home.parents or not (home / "os-home").is_dir():
        raise RuntimeError("a disposable make-home.sh home is required")
    registration = json.loads((home / "agents/prod-shaped/.mcp.json").read_text())
    servers = registration["mcpServers"]
    server = servers.get("duduclaw") or servers["duduclaw-pro"]
    original = (home / "config.toml").read_text()
    # Boot may have saved the default delegation section beside the MCP keys.
    # Replace that section instead of creating a duplicate TOML table.
    base_config = re.sub(r"(?ms)^\[delegation\][^\n]*\n.*?(?=^\[|\Z)", "", original)

    def call(principal, tool, arguments):
        env = {k: os.environ[k] for k in ("PATH", "TMPDIR", "LANG") if k in os.environ}
        env.update(server.get("env", {}))
        env.update(HOME=str(home / "os-home"), DUDUCLAW_HOME=str(home))
        env.pop("DUDUCLAW_ON_BEHALF_OF", None)
        if principal is not None:
            env["DUDUCLAW_ON_BEHALF_OF"] = principal
        with subprocess.Popen(
            [str(binary), *server.get("args", ["mcp-server"])],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env=env, text=True, bufsize=1,
        ) as process:
            lines = queue.Queue()

            def read():
                for line in process.stdout:
                    lines.put(line)
                lines.put(None)

            threading.Thread(target=read, daemon=True).start()

            def rpc(identifier, method, params):
                process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": identifier,
                                                "method": method, "params": params}) + "\n")
                process.stdin.flush()
                while True:
                    line = lines.get(timeout=60)
                    if line is None:
                        raise RuntimeError("MCP server exited")
                    reply = json.loads(line)
                    if reply.get("id") == identifier:
                        if "error" in reply:
                            raise RuntimeError(str(reply["error"]))
                        return reply["result"]

            try:
                rpc(1, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                       "clientInfo": {"name": "on-behalf-of-live", "version": "1"}})
                process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
                process.stdin.flush()
                return rpc(2, "tools/call", {"name": tool, "arguments": arguments})
            finally:
                process.stdin.close()
                process.terminate()
                process.wait(timeout=10)

    def mode(value):
        (home / "config.toml").write_text(
            base_config + f'\n[delegation]\npolicy = "open"\nhuman_principal = "{value}"\n'
        )

    def task(title):
        with sqlite3.connect(home / "tasks.db") as db:
            return db.execute("SELECT on_behalf_of, created_via FROM tasks WHERE title = ?",
                              (title,)).fetchone()

    try:
        mode("record")
        result = call("user:live-account", "tasks_create", {
            "title": "obo-live-record", "assigned_to": "prod-shaped",
            "on_behalf_of": "user:forged", "created_via": "forged",
        })
        assert not result.get("isError"), result
        assert task("obo-live-record") == ("user:live-account", "agent")
        print("PASS native tasks_create stamps host identity and ignores forged arguments")

        result = call("user:live-account", "send_to_agent", {
            "agent_id": "plain", "prompt": "obo-live-message", "on_behalf_of": "user:forged",
        })
        assert not result.get("isError"), result
        with sqlite3.connect(home / "message_queue.db") as db:
            actual = db.execute("SELECT on_behalf_of FROM message_queue WHERE payload = ?",
                                ("obo-live-message",)).fetchone()
        assert actual == ("user:live-account",), actual
        print("PASS native send_to_agent persists host identity on the queue")

        result = call("user:dashboard", "tasks_create", {
            "title": "obo-live-rejected", "assigned_to": "prod-shaped",
        })
        assert not result.get("isError"), result
        assert task("obo-live-rejected") == (None, None)
        audit = [json.loads(line) for line in (home / "security_audit.jsonl").read_text().splitlines()]
        assert any(row.get("event_type") == "on_behalf_of_rejected" for row in audit)
        print("PASS malformed system identity is dropped and audited in record mode")

        mode("enforce")
        for principal in ("user:live-account", "user:dashboard", "unmapped:line:0123456789abcdef"):
            result = call(principal, "tasks_create", {
                "title": "obo-live-denied", "assigned_to": "prod-shaped",
            })
            assert result.get("isError"), result
            assert task("obo-live-denied") is None
        print("PASS enforce rejects unknown, malformed and unmapped task creators")

        result = call(None, "tasks_create", {
            "title": "obo-live-system", "assigned_to": "prod-shaped",
        })
        assert result.get("isError"), result
        assert task("obo-live-system") is None
        print("PASS absent host context cannot claim background authority in enforce mode")
    finally:
        (home / "config.toml").write_text(original)


if __name__ == "__main__":
    main()
