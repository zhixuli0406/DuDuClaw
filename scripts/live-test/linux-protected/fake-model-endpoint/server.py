"""Temporary fixed-reply acceptance endpoint; TLS terminates at Cloud Run."""

import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import sys


COMPLETION_PATH = "/v1/chat/completions"
MAX_BODY = 1024 * 1024
ERRORS = {
    400: b'{"error":"bad_request"}',
    401: b'{"error":"unauthorized"}',
    404: b'{"error":"not_found"}',
    405: b'{"error":"method_not_allowed"}',
    411: b'{"error":"length_required"}',
    413: b'{"error":"payload_too_large"}',
}


def build_response():
    """Return fresh fixed JSON accepted by the worker's common validator."""
    return {
        "choices": [{
            "message": {"role": "assistant", "content": "DUDUCLAW-ACCEPTANCE-OK"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }


def validate_token(token):
    if not token or len(token) < 32:
        raise ValueError("ENDPOINT_TOKEN must contain at least 32 characters")
    return token


def reject_constant(value):
    raise ValueError("Invalid JSON constant")


class AcceptanceServer(ThreadingHTTPServer):
    def handle_error(self, request, client_address):
        # Never print exception traces containing request data.
        pass


class Handler(BaseHTTPRequestHandler):
    # One request per connection, including rejected bodies we do not consume.
    protocol_version = "HTTP/1.0"

    def setup(self):
        self.request.settimeout(15)
        self.request_bytes = 0
        self.response_bytes = 0
        self.response_status = 400
        super().setup()

    def log_message(self, format, *args):
        # Ignore stdlib messages: they can contain headers or raw request lines.
        record = {
            "method": getattr(self, "command", None),
            "path": getattr(self, "path", "").split("?", 1)[0].split("#", 1)[0],
            "status": self.response_status,
            "request_bytes": self.request_bytes,
            "response_bytes": self.response_bytes,
        }
        print(json.dumps(record, ensure_ascii=True), file=sys.stderr, flush=True)

    def reply(self, status, body=None, content_type="application/json"):
        if body is None:
            body = ERRORS[status]
        self.close_connection = True
        self.response_status = status
        self.response_bytes = 0 if getattr(self, "command", None) == "HEAD" else len(body)
        self.send_response_only(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.log_message("")
        if self.response_bytes:
            self.wfile.write(body)

    def send_error(self, code, message=None, explain=None):
        # BaseHTTPRequestHandler otherwise reflects malformed input into HTML.
        self.reply(code if code in ERRORS else 400)

    def __getattr__(self, name):
        # Apply the same authentication and method rules to unknown verbs.
        if name.startswith("do_"):
            return self.handle_endpoint
        raise AttributeError(name)

    def handle_endpoint(self):
        if self.command == "GET" and self.path == "/healthz":
            self.reply(200, b"ok", "text/plain")
            return
        authorization = self.headers.get_all("Authorization", [])
        if (len(authorization) != 1 or not hmac.compare_digest(
                authorization[0].encode("utf-8"), self.server.authorization)):
            self.reply(401)
            return
        if self.path not in (COMPLETION_PATH, "/healthz"):
            self.reply(404)
            return
        if self.path != COMPLETION_PATH or self.command != "POST":
            self.reply(405)
            return
        if self.headers.get_all("Transfer-Encoding") is not None:
            self.reply(400)
            return
        lengths = self.headers.get_all("Content-Length", [])
        if not lengths:
            self.reply(411)
            return
        if (len(lengths) != 1 or not lengths[0].isascii()
                or not lengths[0].isdigit()):
            self.reply(400)
            return
        # Compare decimal strings before conversion, including huge integers.
        length_text = lengths[0].lstrip("0") or "0"
        if len(length_text) > len(str(MAX_BODY)) or (
                len(length_text) == len(str(MAX_BODY)) and length_text > str(MAX_BODY)):
            self.reply(413)
            return
        length = int(length_text)
        if self.headers.get_all("Content-Type", []) != ["application/json"]:
            self.reply(400)
            return
        try:
            body = self.rfile.read(length)
            self.request_bytes = len(body)
            if len(body) != length:
                raise ValueError("Incomplete body")
            value = json.loads(body.decode("utf-8"), parse_constant=reject_constant)
            if not isinstance(value, dict):
                raise ValueError("Object required")
        except (ValueError, RecursionError, OSError):
            self.reply(400)
            return
        self.reply(200, json.dumps(build_response(), separators=(",", ":")).encode("utf-8"))


def create_server(token, address=("0.0.0.0", 8080)):
    authorization = ("Bearer " + validate_token(token)).encode("utf-8")
    httpd = AcceptanceServer(address, Handler)
    httpd.authorization = authorization
    return httpd


def main():
    try:
        token = validate_token(os.environ.get("ENDPOINT_TOKEN"))
    except ValueError as error:
        print(str(error), file=sys.stderr)
        return 1
    with create_server(token, ("0.0.0.0", int(os.environ.get("PORT", "8080")))) as httpd:
        httpd.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
