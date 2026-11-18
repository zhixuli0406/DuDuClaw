import contextlib
import http.client
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest

import server


TOKEN = "test-only-" + "a" * 40
PATH = "/v1/chat/completions"


class EndpointTests(unittest.TestCase):
    def setUp(self):
        self.logs = io.StringIO()
        self.capture = contextlib.redirect_stderr(self.logs)
        self.capture.__enter__()
        self.addCleanup(self.capture.__exit__, None, None, None)
        self.httpd = server.create_server(TOKEN, ("127.0.0.1", 0))
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join()

    def request(self, method="POST", path=PATH, body=b"{}", headers=None):
        actual = {"Authorization": "Bearer " + TOKEN,
                  "Content-Type": "application/json"}
        if headers:
            for key, value in headers.items():
                if value is None:
                    actual.pop(key, None)
                else:
                    actual[key] = value
        with contextlib.closing(http.client.HTTPConnection(
                "127.0.0.1", self.httpd.server_port, timeout=3)) as conn:
            conn.request(method, path, body=body, headers=actual)
            response = conn.getresponse()
            return response.status, response.read()

    def test_missing_and_wrong_tokens(self):
        for token in (None, "Bearer wrong", "Bearer \u00e9"):
            with self.subTest(token=token):
                self.assertEqual(self.request(headers={"Authorization": token}),
                                 (401, b'{"error":"unauthorized"}'))

    def test_wrong_path(self):
        for path in ("/chat/completions", PATH + "/", PATH + "?x=1"):
            self.assertEqual(self.request(path=path)[0], 404)

    def test_wrong_methods(self):
        for method in ("GET", "PUT", "DELETE", "OPTIONS", "CUSTOM"):
            self.assertEqual(self.request(method=method)[0], 405)

    def test_health_is_only_unauthenticated_route(self):
        self.assertEqual(self.request("GET", "/healthz", headers={"Authorization": None}),
                         (200, b"ok"))
        for method, path in (("POST", "/healthz"), ("GET", "/healthz?x=1"),
                             ("GET", "/missing")):
            self.assertEqual(self.request(method, path, headers={"Authorization": None})[0], 401)

    def test_oversize(self):
        self.assertEqual(self.request(headers={"Content-Length": str(1024 * 1024 + 1)})[0], 413)

    def test_non_json_and_non_objects(self):
        for body in (b"not JSON", b"[]", b"null", b'"text"', b"{\xff}", b'{"x":NaN}'):
            self.assertEqual(self.request(body=body)[0], 400)

    def test_wrong_content_type(self):
        for content_type in (None, "text/plain"):
            self.assertEqual(self.request(headers={"Content-Type": content_type})[0], 400)

    def test_no_chunked_or_invalid_lengths(self):
        self.assertEqual(self.request(headers={"Transfer-Encoding": "chunked"})[0], 400)
        self.assertEqual(self.request(headers={"Content-Length": "-1"})[0], 400)
        self.assertEqual(self.request(headers={"Content-Length": "invalid"})[0], 400)
        self.assertEqual(self.request("POST", body=None)[0], 400)  # Empty body, length 0.
        with contextlib.closing(http.client.HTTPConnection(
                "127.0.0.1", self.httpd.server_port, timeout=3)) as conn:
            conn.putrequest("POST", PATH)
            conn.putheader("Authorization", "Bearer " + TOKEN)
            conn.putheader("Content-Type", "application/json")
            conn.endheaders()
            response = conn.getresponse()
            self.assertEqual(response.status, 411)
            response.read()

    def test_good_request_and_no_request_echo(self):
        status, body = self.request(body=b'{"model":"private-model","messages":[],"stream":false}')
        self.assertEqual(status, 200)
        value = json.loads(body)
        self.assertEqual(value, {
            "choices": [{"message": {"role": "assistant", "content": "DUDUCLAW-ACCEPTANCE-OK"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })
        self.assertNotIn(b"private-model", body)

    def test_maximum_body_size(self):
        body = b'{"padding":"' + b"x" * (1024 * 1024 - 14) + b'"}'
        self.assertEqual(len(body), 1024 * 1024)
        self.assertEqual(self.request(body=body)[0], 200)

    def test_logs_exclude_body_headers_token_and_query(self):
        marker = "PRIVATE-MARKER"
        self.request(body=json.dumps({"message": marker}).encode(), headers={"X-Test": marker})
        self.request(path=PATH + "?secret=" + marker)
        output = self.logs.getvalue()
        self.assertNotIn(marker, output)
        self.assertNotIn(TOKEN, output)
        self.assertNotIn("?", output)
        records = [json.loads(line) for line in output.splitlines()]
        self.assertEqual(len(records), 2)
        for record in records:
            self.assertEqual(set(record), {"method", "path", "status", "request_bytes", "response_bytes"})
            self.assertEqual(record["path"], PATH)
        self.assertEqual(records[0]["status"], 200)
        self.assertGreater(records[0]["request_bytes"], 0)

    def test_response_builder_is_pure(self):
        first = server.build_response()
        first["choices"].clear()
        self.assertEqual(server.build_response()["choices"][0]["message"]["content"],
                         "DUDUCLAW-ACCEPTANCE-OK")


class StartupTests(unittest.TestCase):
    def test_missing_and_short_tokens_refuse_startup(self):
        for token in (None, "short-private-token"):
            env = dict(os.environ)
            env.pop("ENDPOINT_TOKEN", None)
            if token is not None:
                env["ENDPOINT_TOKEN"] = token
            result = subprocess.run([sys.executable, str(Path(__file__).with_name("server.py"))],
                                    env=env, capture_output=True, timeout=3)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"ENDPOINT_TOKEN", result.stderr)
            self.assertNotIn(b"short-private-token", result.stderr)


if __name__ == "__main__":
    unittest.main()
