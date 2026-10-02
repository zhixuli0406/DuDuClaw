"""duduclaw_cdp: the Chrome DevTools Protocol plumbing shared by
duduclaw-eval-dom and duduclaw-navigate (installed next to them in
/usr/local/bin, which is on their import path as the scripts' directory).

Network: connects only to 127.0.0.1:<DUDUCLAW_CDP_PORT, default 9222>; a
WebSocket path is taken from the target list but the host never is.
Standard library only.

Isolation: every expression runs in a fresh isolated world on the page's main
frame (`evaluate_isolated`), never in the page's own JavaScript world. An
isolated world shares the DOM but has its own globals and prototypes, so a
hostile page that redefines `document.visibilityState`,
`Document.prototype.querySelectorAll`, `Element.prototype.getBoundingClientRect`
or `JSON.stringify` in its world does not change what the helpers read.
"""

import base64
import http.client
import json
import os
import socket
import struct
from urllib.parse import urlsplit

CDP_HOST = "127.0.0.1"
CDP_PORT = int(os.environ.get("DUDUCLAW_CDP_PORT", "9222"))
MAX_MESSAGE_BYTES = 8 * 1024 * 1024
MAX_PAGES = 16


class CdpError(Exception):
    pass


PAGE_PATH_PREFIX = "/devtools/page/"


def _http_get(path, timeout):
    """GET one DevTools HTTP endpoint on loopback; (status, body)."""
    conn = http.client.HTTPConnection(CDP_HOST, CDP_PORT, timeout=timeout)
    try:
        conn.request("GET", path, headers={"Host": f"{CDP_HOST}:{CDP_PORT}"})
        resp = conn.getresponse()
        return resp.status, resp.read(MAX_MESSAGE_BYTES)
    except OSError as e:
        raise CdpError(f"browser DevTools endpoint unreachable on {CDP_HOST}:{CDP_PORT}: {e}")
    finally:
        conn.close()


def page_target_id(path):
    """The target id in a `/devtools/page/<id>` path (validated by list_pages)."""
    if not path.startswith(PAGE_PATH_PREFIX):
        raise CdpError("not a page target path")
    target_id = path[len(PAGE_PATH_PREFIX):]
    if not target_id or not all(c.isalnum() or c in "-_" for c in target_id):
        raise CdpError("bad page target id")
    return target_id


def close_page(path, timeout):
    """Close one page target through the DevTools HTTP endpoint."""
    status, _ = _http_get(f"/json/close/{page_target_id(path)}", timeout)
    if status != 200:
        raise CdpError(f"/json/close returned HTTP {status}")


def activate_page(path, timeout):
    """Bring one page target to the front of its window."""
    status, _ = _http_get(f"/json/activate/{page_target_id(path)}", timeout)
    if status != 200:
        raise CdpError(f"/json/activate returned HTTP {status}")


def evaluate_isolated(ws, expression):
    """Runtime.evaluate `expression` in a new isolated world on the page's
    main frame (see the module docstring); returns the evaluate result."""
    tree = ws.call("Page.getFrameTree")
    frame_id = tree.get("frameTree", {}).get("frame", {}).get("id")
    if not isinstance(frame_id, str) or not frame_id:
        raise CdpError("page has no main frame")
    world = ws.call("Page.createIsolatedWorld", {
        "frameId": frame_id, "worldName": "duduclaw", "grantUniveralAccess": False,
    })
    context_id = world.get("executionContextId")
    if not isinstance(context_id, int) or isinstance(context_id, bool):
        raise CdpError("isolated world was not created")
    return ws.call("Runtime.evaluate", {
        "expression": expression, "contextId": context_id,
        "returnByValue": True, "awaitPromise": False, "userGesture": False,
        "includeCommandLineAPI": False, "silent": True,
    })


def list_page_targets(timeout):
    """(WebSocket path, URL) of the open page targets (devtools pages
    excluded)."""
    status, body = _http_get("/json/list", timeout)
    if status != 200:
        raise CdpError(f"/json/list returned HTTP {status}")
    targets = json.loads(body)
    pages = []
    for t in targets:
        if t.get("type") != "page":
            continue
        url = str(t.get("url", ""))
        if url.startswith("devtools://"):
            continue
        ws_url = t.get("webSocketDebuggerUrl")
        if not ws_url:
            continue
        # Use the path only; the host is always loopback.
        marker = PAGE_PATH_PREFIX
        idx = ws_url.find(marker)
        if idx < 0:
            continue
        path = ws_url[idx:]
        if not all(c.isalnum() or c in "/-_" for c in path):
            continue
        pages.append((path, url))
    return pages[:MAX_PAGES]


def is_browser_ui(url):
    """True for Chromium's own top-chrome WebUI (e.g. the omnibox popup on
    chrome://omnibox-popup.top-chrome/), which is listed as a "page" target
    but is part of the browser window, not a tab: it is still evaluated for
    visibility (a visible one means a non-kiosk window is open) but never
    closed or navigated."""
    try:
        parts = urlsplit(url)
    except ValueError:
        return False
    host = (parts.hostname or "").lower()
    return parts.scheme in ("chrome", "chrome-untrusted") and (
        host == "top-chrome" or host.endswith(".top-chrome")
    )


def list_pages(timeout):
    """WebSocket paths of the open page targets (devtools pages excluded)."""
    return [path for path, _ in list_page_targets(timeout)]


class WebSocket:
    """Minimal RFC 6455 client: text frames, client masking, no extensions."""

    def __init__(self, path, timeout):
        self.sock = socket.create_connection((CDP_HOST, CDP_PORT), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {CDP_HOST}:{CDP_PORT}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(req.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise CdpError("websocket handshake: connection closed")
            head += chunk
            if len(head) > 65536:
                raise CdpError("websocket handshake: oversized response")
        status_line = head.split(b"\r\n", 1)[0]
        if b" 101 " not in status_line + b" ":
            raise CdpError(f"websocket handshake refused: {status_line.decode(errors='replace')}")
        self.buf = head.split(b"\r\n\r\n", 1)[1]
        self.next_id = 0

    def settimeout(self, seconds):
        self.sock.settimeout(seconds)

    def _recv_exact(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(max(65536, n - len(self.buf)))
            if not chunk:
                raise CdpError("websocket closed by browser")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _send_frame(self, opcode, payload):
        header = bytes([0x80 | opcode])
        n = len(payload)
        if n < 126:
            header += bytes([0x80 | n])
        elif n < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", n)
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", n)
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def send_text(self, text):
        self._send_frame(0x1, text.encode())

    def recv_message(self):
        parts = []
        total = 0
        while True:
            b0, b1 = self._recv_exact(2)
            fin, opcode = b0 & 0x80, b0 & 0x0F
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack(">H", self._recv_exact(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", self._recv_exact(8))[0]
            if b1 & 0x80:
                mask = self._recv_exact(4)
            else:
                mask = None
            total += n
            if total > MAX_MESSAGE_BYTES:
                raise CdpError("CDP message too large")
            payload = self._recv_exact(n)
            if mask:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if opcode == 0x8:
                raise CdpError("websocket closed by browser")
            if opcode == 0x9:
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            parts.append(payload)
            if fin:
                return b"".join(parts).decode()

    def send_command(self, method, params=None):
        """Send one command; returns its id (answers come via recv_json)."""
        self.next_id += 1
        self.send_text(json.dumps({"id": self.next_id, "method": method, "params": params or {}}))
        return self.next_id

    def recv_json(self):
        return json.loads(self.recv_message())

    def call(self, method, params=None):
        """Send one command and wait for its answer, skipping events.
        Raises CdpError on a protocol error."""
        msg_id = self.send_command(method, params)
        while True:
            msg = self.recv_json()
            if msg.get("id") == msg_id:
                if "error" in msg:
                    raise CdpError(f"CDP error: {msg['error'].get('message', msg['error'])}")
                return msg.get("result", {})

    def close(self):
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        self.sock.close()
