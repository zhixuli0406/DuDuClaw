"""Zero-provider probe: local HTTP fixtures exercise the real adapter script."""
import http.server
import json
import os
from pathlib import Path
import socketserver
import socketserver
import subprocess
import tempfile
import threading

SCRIPT = Path(__file__).with_name("attempt_openai_compat.py")


def fixture(mode, cwd):
    class Handler(http.server.BaseHTTPRequestHandler):
        calls = []
        def log_message(self, *_args):
            pass
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            self.calls.append(body)
            assert self.headers["Authorization"] == "Bearer synthetic-only"
            assert {tool["function"]["name"] for tool in body["tools"]} == {"read_file", "write_file", "list_directory", "shell"}
            if mode == "limit":
                self.send_response(429); self.end_headers(); return
            if mode == "redirect":
                self.send_response(302); self.send_header("Location", "/secret-leak"); self.end_headers(); return
            if mode == "tools" and len(self.calls) == 1:
                message = {"role": "assistant", "tool_calls": [{"id": "write", "type": "function", "function": {"name": "write_file", "arguments": json.dumps({"path": "actual.txt", "content": "actual output"})}}]}
            else:
                message = {"role": "assistant", "content": "done"}
            usage = {"prompt_tokens": 10, "completion_tokens": 3, "prompt_tokens_details": {"cached_tokens": 2}}
            if mode == "unknown":
                del usage["completion_tokens"]
            encoded = json.dumps({"model": "observed-test-model", "usage": usage, "choices": [{"message": message}]}).encode()
            self.send_response(200); self.end_headers(); self.wfile.write(encoded)
    class LocalServer(http.server.HTTPServer):
        def server_bind(self):
            socketserver.TCPServer.server_bind(self)
            self.server_name = "localhost"
            self.server_port = self.server_address[1]
    server = LocalServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
    env = {"PATH": "/usr/bin:/bin", "OPENAI_API_KEY": "synthetic-only", "DUDU_ATTEMPT_BASE_URL": "http://127.0.0.1:%s/v1" % server.server_port}
    try:
        result = subprocess.run(["python3", "-I", "-S", "-B", str(SCRIPT), "configured-model", "3"], input="synthetic prompt", text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=cwd, env=env, timeout=10)
        events = [json.loads(line) for line in result.stdout.splitlines()]
        if mode == "tools":
            assert result.returncode == 0 and len(Handler.calls) == 2
            assert Path(cwd, "actual.txt").read_text() == "actual output"
            assert events[-1]["usage"] == {"input_tokens": 16, "output_tokens": 6, "cache_read_input_tokens": 4}
            assert events[0]["message"]["model"] == "observed-test-model"
        elif mode == "unknown":
            assert result.returncode == 0 and events[-1]["usage"] is None
        else:
            assert result.returncode == 1 and len(Handler.calls) == 1
            assert events[-1]["is_error"] and events[-1]["error"]["code"] == (429 if mode == "limit" else 302)
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=2)


with tempfile.TemporaryDirectory(prefix="dudu-attempt-zero-provider-") as private:
    for mode in ("tools", "unknown", "limit", "redirect"):
        fixture(mode, private)
print("4 local HTTP adapter probes passed; zero provider calls")
