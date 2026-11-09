#!/usr/bin/env python3
"""Public HTTPS fixture origin + fixture skill hub for package-h-acceptance.sh
(stdlib only, Python 3.10+).

Serves, on one TLS listener (certificate from the harness' throw-away CA):

  GET /h15/ok                 200 text "h15-public-ok"
  GET /h15/page.html          200 HTML with <p id="h15">h15-extract-target</p>
  GET /h15/redirect           302 -> https://127.0.0.1/h15/ok   (must NOT be followed)
  GET /api/v1/skills/<slug>   ClawHub-shaped detail JSON (moderation verdict per slug)
  GET /api/v1/download?slug=  zip archive per slug:
        h15-good       one SKILL.md (name = slug)                    -> installs
        h15-bomb       SKILL.md + 32 MiB of zeros (ratio > 100)      -> refused
        h15-traversal  SKILL.md + "../h15-escape"                    -> refused
        h15-symlink    SKILL.md + symlink entry -> /etc/passwd       -> refused
        h15-flagged    clean archive, detail says isSuspicious=true  -> refused (layer 1)
  GET /__hits                 {"hits": N} (not logged)

Every request is appended to --log as one JSON line (method, host, path, no
header values). The hub endpoint shapes follow `ClawHubHub::fetch_manifest`
and `hub_install_owned::archive_url`; H12b/H13c note they must be verified
against the live hub, so a refusal of h15-good can also mean a shape drift.
"""
import argparse
import io
import json
import ssl
import stat
import threading
import time
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

LOCK = threading.Lock()
HITS = 0
SKILL = "---\nname: {slug}\ndescription: H15 acceptance fixture skill\n---\n\n# {slug}\n\nSay hello.\n"


def archive(slug):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(f"{slug}/SKILL.md", SKILL.format(slug=slug))
        if slug == "h15-bomb":
            zf.writestr(f"{slug}/zeros.bin", b"\0" * (32 << 20))
        elif slug == "h15-traversal":
            info = zipfile.ZipInfo("../h15-escape")
            zf.writestr(info, "escaped\n")
        elif slug == "h15-symlink":
            info = zipfile.ZipInfo(f"{slug}/link")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            zf.writestr(info, "/etc/passwd")
    return buffer.getvalue()


def detail(slug):
    flagged = slug == "h15-flagged"
    return {"skill": {"slug": slug, "displayName": slug, "isSuspicious": flagged,
                      "owner": {"handle": "h15"}},
            "moderation": {"status": "flagged" if flagged else "clean",
                           "verdict": "malicious" if flagged else "clean"}}


def make_handler(log_path):
    class Handler(BaseHTTPRequestHandler):
        server_version = "iso-h15-origin"

        def log_message(self, *_args):
            return

        def _send(self, status, body, content_type, headers=()):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            for name, value in headers:
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            global HITS
            url = urlsplit(self.path)
            if url.path == "/__hits":
                with LOCK:
                    self._send(200, json.dumps({"hits": HITS}).encode(), "application/json")
                return
            with LOCK:
                HITS += 1
                with open(log_path, "a", encoding="utf-8") as log:
                    log.write(json.dumps({"ts": round(time.time(), 3), "method": "GET",
                                          "host": self.headers.get("Host", ""), "path": url.path}) + "\n")
            if url.path == "/h15/ok":
                self._send(200, b"h15-public-ok\n", "text/plain; charset=utf-8")
            elif url.path == "/h15/page.html":
                self._send(200, b"<!doctype html><html><body><p id=\"h15\">h15-extract-target</p></body></html>",
                           "text/html; charset=utf-8")
            elif url.path == "/h15/redirect":
                self._send(302, b"", "text/plain", [("Location", "https://127.0.0.1/h15/ok")])
            elif url.path.startswith("/api/v1/skills/"):
                slug = url.path.rsplit("/", 1)[-1]
                if not slug.startswith("h15-"):
                    self._send(404, b"{}", "application/json")
                else:
                    self._send(200, json.dumps(detail(slug)).encode(), "application/json")
            elif url.path == "/api/v1/download":
                slug = (parse_qs(url.query).get("slug") or [""])[0]
                if not slug.startswith("h15-"):
                    self._send(404, b"", "application/zip")
                else:
                    self._send(200, archive(slug), "application/zip")
            else:
                self._send(404, b"not found\n", "text/plain")

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bind", required=True)
    parser.add_argument("--port", type=int, default=443)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    parser.add_argument("--log", required=True)
    args = parser.parse_args()
    open(args.log, "a", encoding="utf-8").close()
    server = ThreadingHTTPServer((args.bind, args.port), make_handler(args.log))
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.cert, args.key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
