#!/usr/bin/env python3
"""Local HTTPS canary origin for connectors-acceptance.sh (stdlib only).

Stands in for a provider endpoint (Odoo, a channel API, a search API) so a
check can prove that a refused call never reached the network, and in
particular never carried a credential: every request is appended to --log as
one JSON line

    {"ts": ..., "method": "POST", "path": "/jsonrpc", "authorization": bool,
     "cookie": bool, "api_key_header": bool, "body_bytes": 12}

Credential values are never recorded, only whether a credential-bearing header
was present. GET /__hits answers {"hits": N} without being logged. Every other
path answers 200 {"ok": true}. The certificate is self-signed; the harness
creates it with openssl and only uses this origin where the product's own
configuration points a fixture provider at it.
"""
import argparse
import json
import ssl
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
HITS = 0


def make_handler(log_path):
    class Handler(BaseHTTPRequestHandler):
        server_version = "iso-canary"

        def log_message(self, *_args):  # keep stderr quiet
            return

        def _answer(self, body):
            data = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def _handle(self):
            global HITS
            length = int(self.headers.get("Content-Length") or 0)
            if length:
                self.rfile.read(min(length, 1 << 20))
            if self.path == "/__hits" and self.command == "GET":
                with LOCK:
                    self._answer({"hits": HITS})
                return
            entry = {
                "ts": round(time.time(), 3), "method": self.command, "path": self.path.split("?", 1)[0],
                "authorization": "Authorization" in self.headers,
                "cookie": "Cookie" in self.headers,
                "api_key_header": any(name.lower().endswith(("api-key", "token")) for name in self.headers.keys()),
                "body_bytes": length,
            }
            with LOCK:
                HITS += 1
                with open(log_path, "a", encoding="utf-8") as log:
                    log.write(json.dumps(entry) + "\n")
            self._answer({"ok": True})

        do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = _handle

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()
    open(args.log, "a", encoding="utf-8").close()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), make_handler(args.log))
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.cert, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
