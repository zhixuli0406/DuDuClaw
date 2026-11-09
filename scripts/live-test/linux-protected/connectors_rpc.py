#!/usr/bin/env python3
"""Dashboard RPC driver for connectors-acceptance.sh (stdlib only, Python 3.10+).

Reuses the WebSocket client of rpc_probe.py. Unlike rpc_probe.py (which uses the
legacy admin token), the protected connector/employee RPCs need a real account
session: JWT login, optional TOTP enrolment and the dashboard step-up
(`isolation.connector_step_up`, password + a fresh TOTP code, 5 minutes).

Every sub-command prints exactly one JSON object on stdout and never prints a
password, token or TOTP secret:

    {"ok": bool, "payload": ..., "error": str|null, "elapsed_ms": int}

Exit status: 0 when the command ran (inspect "ok"), 2 on a usage/transport
problem. State (credentials, tokens, TOTP secret) lives in the 0600 --state file
that the caller keeps in a private temporary directory.

Request parameters may contain the string "@csrf" anywhere a CSRF token is
expected: it is replaced by the 32-integer array of the current step-up proof.
"""
import argparse
import base64
import hashlib
import hmac
import importlib.util
import json
import os
import struct
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("rpc_probe", HERE / "rpc_probe.py")
probe = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(probe)

PERIOD = 30


class Fail(Exception):
    pass


# ── state ────────────────────────────────────────────────────────────────────
def load_state(path):
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}


def save_state(path, state):
    tmp = Path(str(path) + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as out:
        json.dump(state, out)
    os.replace(tmp, path)


# ── TOTP (RFC 6238, SHA-1, 6 digits, 30 s) ───────────────────────────────────
def totp_code(secret, counter):
    key = base64.b32decode(secret.replace(" ", "").upper() + "=" * (-len(secret) % 8))
    digest = hmac.new(key, struct.pack(">Q", counter), hashlib.sha1).digest()
    offset = digest[-1] & 15
    value = (struct.unpack(">I", digest[offset:offset + 4])[0] & 0x7FFFFFFF) % 1_000_000
    return f"{value:06d}"


def fresh_code(state, path):
    """A code for a time step not used before (the server refuses replays)."""
    secret = state.get("totp_secret")
    if not secret:
        raise Fail("account has no TOTP secret (enrol first)")
    counter = int(time.time()) // PERIOD
    last = state.get("totp_last", -1)
    if counter <= last:
        time.sleep((last + 1) * PERIOD - time.time() + 0.5)
        counter = int(time.time()) // PERIOD
    state["totp_last"] = counter
    save_state(path, state)
    return totp_code(secret, counter)


# ── HTTP ─────────────────────────────────────────────────────────────────────
def http_base():
    url = probe.default_url()
    return url.replace("ws://", "http://", 1).rsplit("/ws", 1)[0]


def http_json(method, path, body=None, bearer=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(http_base() + path, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if bearer:
        request.add_header("Authorization", "Bearer " + bearer)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read()
            status = response.status
    except urllib.error.HTTPError as error:
        raw = error.read()
        status = error.code
    except OSError as error:
        raise Fail(f"HTTP transport error: {error}") from error
    try:
        return status, json.loads(raw.decode("utf-8")) if raw else None
    except ValueError:
        return status, None


def login(state, path, new_password=None):
    status, body = http_json("POST", "/api/login", {"email": state["email"], "password": state["password"]})
    if status != 200 or not isinstance(body, dict) or not body.get("access_token"):
        keys = sorted(body) if isinstance(body, dict) else None
        raise Fail(f"login did not return a session (HTTP {status}, keys {keys}); "
                   "an enforced MFA policy needs a pre-enrolled account")
    state["access_token"], state["refresh_token"] = body["access_token"], body.get("refresh_token")
    user = body.get("user") or {}
    state["user_id"] = user.get("id", state.get("user_id"))
    if user.get("must_change_password"):
        target = new_password or state["password"] + "-x1"
        status, _ = http_json("POST", "/api/change-password", {"new_password": target}, bearer=state["access_token"])
        if status != 200:
            raise Fail(f"forced password change failed (HTTP {status})")
        state["password"] = target
        save_state(path, state)
        return login(state, path)
    save_state(path, state)
    return state


def refresh(state, path):
    if not state.get("refresh_token"):
        return False
    status, body = http_json("POST", "/api/refresh", {"refresh_token": state["refresh_token"]})
    if status != 200 or not isinstance(body, dict) or not body.get("access_token"):
        return False
    state["access_token"] = body["access_token"]
    state["refresh_token"] = body.get("refresh_token", state["refresh_token"])
    save_state(path, state)
    return True


# ── WebSocket session ────────────────────────────────────────────────────────
class Session:
    def __init__(self, state, path):
        self.state, self.path, self.serial = state, path, 0
        self.ws = None
        self._connect()

    def _open(self):
        ws = probe.WebSocket(probe.default_url())
        self.serial += 1
        ws.call(f"c{self.serial}", "connect", {"jwt": self.state.get("access_token", ""), "client": "acceptance"})
        return ws

    def _connect(self):
        try:
            self.ws = self._open()
        except probe.ProbeError:
            if not refresh(self.state, self.path):
                login(self.state, self.path)
            self.ws = self._open()

    def call(self, method, params):
        self.serial += 1
        return self.ws.call(f"r{self.serial}", method, params)

    def close(self):
        if self.ws:
            self.ws.close()


def substitute(value, csrf):
    if value == "@csrf":
        if csrf is None:
            raise Fail("request needs a CSRF token but no step-up proof is available")
        return csrf
    if isinstance(value, dict):
        return {k: substitute(v, csrf) for k, v in value.items()}
    if isinstance(value, list):
        return [substitute(v, csrf) for v in value]
    return value


def step_up(session, state, path):
    """Return the CSRF token of a valid step-up proof, verifying first if needed."""
    for attempt in range(2):
        listing = session.call("isolation.employee_list", {})
        if listing.get("step_up_valid") and listing.get("csrf_token"):
            return listing["csrf_token"]
        if attempt == 1:
            break
        session.call("isolation.connector_step_up",
                     {"password": state["password"], "code": fresh_code(state, path)})
    raise Fail("step-up proof not available after verification")


# ── commands ─────────────────────────────────────────────────────────────────
def cmd_wait(args):
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        try:
            status, _ = http_json("GET", "/api/first-run/status")
            if status == 200:
                return None
        except Fail:
            pass
        time.sleep(1)
    raise Fail("gateway did not answer /api/first-run/status in time")


def cmd_claim(args):
    state = load_state(args.state)
    status, body = http_json("GET", "/api/first-run/status")
    if status != 200 or not (body or {}).get("claimable"):
        raise Fail("first-run claim is not available (already claimed?)")
    state.update(email=args.email, password=args.password)
    status, _ = http_json("POST", "/api/first-run/claim", {"email": args.email, "password": args.password})
    if status != 200:
        raise Fail(f"first-run claim refused (HTTP {status})")
    save_state(args.state, state)
    return {"claimed": True}


def cmd_login(args):
    state = load_state(args.state)
    if args.email:
        state.update(email=args.email, password=args.password)
    login(state, args.state, args.new_password)
    return {"user_id": state.get("user_id")}


def cmd_enroll_mfa(args):
    state = load_state(args.state)
    session = Session(state, args.state)
    try:
        begin = session.call("mfa.enroll_begin", {"password": state["password"]})
        if begin.get("proof_required") or not begin.get("secret"):
            raise Fail("MFA enrolment needs an independent proof code (enforced policy); "
                       "enrol this account by hand and see README 'MFA'")
        state["totp_secret"] = begin["secret"]
        state["totp_last"] = -1
        save_state(args.state, state)
        done = session.call("mfa.enroll_confirm",
                            {"enrollment_token": begin["enrollment_token"], "code": fresh_code(state, args.state)})
        if done.get("access_token"):
            state["access_token"], state["refresh_token"] = done["access_token"], done.get("refresh_token")
        state["recovery_codes_count"] = len(done.get("recovery_codes") or [])
        save_state(args.state, state)
    finally:
        session.close()
    return {"enrolled": True}


def cmd_call(args):
    state = load_state(args.state)
    params = json.loads(args.params)
    if not isinstance(params, dict):
        raise Fail("params must be a JSON object")
    session = Session(state, args.state)
    try:
        csrf = step_up(session, state, args.state) if args.stepup else None
        params = substitute(params, csrf)
        started = time.monotonic()
        payload = session.call(args.method, params)
        elapsed = int((time.monotonic() - started) * 1000)
    finally:
        session.close()
    return {"_elapsed_ms": elapsed, "payload": payload}


def cmd_connect_only(args):
    """Open a session with the stored (possibly stale) token and report whether it is accepted."""
    state = load_state(args.state)
    ws = probe.WebSocket(probe.default_url())
    try:
        ws.call("c1", "connect", {"jwt": state.get("access_token", ""), "client": "acceptance"})
    finally:
        ws.close()
    return {"accepted": True}


def _walk(value):
    yield value
    if isinstance(value, dict):
        for item in value.values():
            yield from _walk(item)
    elif isinstance(value, list):
        for item in value:
            yield from _walk(item)


def cmd_find(args):
    """stdin JSON -> first dict having key==value (recursive)."""
    data = json.load(sys.stdin)
    for node in _walk(data):
        if isinstance(node, dict) and str(node.get(args.key)) == args.value:
            print(json.dumps(node))
            return "raw"
    sys.exit(1)


def cmd_contains(args):
    """stdin JSON -> exit 0 if every TEXT occurs (case-insensitive), 1 if not, 2 if stdin is not JSON."""
    try:
        text = json.dumps(json.load(sys.stdin), ensure_ascii=False).lower()
    except ValueError:
        sys.exit(2)
    sys.exit(0 if all(t.lower() in text for t in args.text) else 1)


def cmd_pick_proposal(args):
    """stdin: migration.confirmations.list response -> binding of the newest open Employee proposal for a label."""
    data = json.load(sys.stdin)
    items = (data.get("payload") or data).get("items") or []
    chosen = None
    for item in items:
        if str(item.get("category", "")).lower() != "employee":
            continue
        if str(item.get("status", "")).lower() not in ("pending", "approved", "applying"):
            continue
        if args.label.lower() in json.dumps(item, ensure_ascii=False).lower():
            chosen = item
    if chosen is None:
        sys.exit(1)
    print(json.dumps(chosen["binding"]))
    return "raw"


def cmd_employee(args):
    """stdin: isolation.employee_list response -> the live (not tombstoned) confirmed employee node of a label.

    A confirmed employee node is a dict with `label` == LABEL and an `employee_id`.
    With --all, prints every employee_id ever seen for the label, one per line.
    """
    data = json.load(sys.stdin)
    nodes = [n for n in _walk(data) if isinstance(n, dict) and str(n.get("label")) == args.label
             and n.get("employee_id")]
    if args.all:
        for node in nodes:
            print(node["employee_id"])
        return "raw"

    def dead(node):
        text = json.dumps(node).lower()
        return bool(node.get("tombstoned")) or any(w in str(node.get("status", "")).lower()
                                                   for w in ("tombstone", "disabled")) or "tombstoned\": true" in text
    live = [n for n in nodes if not dead(n)]
    if not live:
        sys.exit(1)
    print(json.dumps(live[-1]))
    return "raw"


def jget(data, path):
    node = data
    for part in [p for p in path.strip(".").split(".") if p]:
        name, _, rest = part.partition("[")
        if name:
            node = node[name]
        while rest:
            selector, _, rest = rest.partition("]")
            rest = rest.lstrip("[")
            if "=" in selector:
                key, value = selector.split("=", 1)
                node = next(x for x in node if str(x.get(key)) == value)
            else:
                node = node[int(selector)]
    return node


def cmd_jget(args):
    try:
        value = jget(json.load(sys.stdin), args.path)
    except (KeyError, IndexError, StopIteration, TypeError, ValueError):
        sys.exit(1)
    print(value if isinstance(value, str) else json.dumps(value))
    return "raw"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--state", default="")
    parser.add_argument("--data-home", default="", help="service DUDUCLAW_HOME (gateway port is read from it)")
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("wait"); p.add_argument("--timeout", type=int, default=90); p.set_defaults(fn=cmd_wait)
    p = sub.add_parser("claim"); p.add_argument("--email", required=True); p.add_argument("--password", required=True); p.set_defaults(fn=cmd_claim)
    p = sub.add_parser("login"); p.add_argument("--email"); p.add_argument("--password"); p.add_argument("--new-password"); p.set_defaults(fn=cmd_login)
    p = sub.add_parser("enroll-mfa"); p.set_defaults(fn=cmd_enroll_mfa)
    p = sub.add_parser("connect-only"); p.set_defaults(fn=cmd_connect_only)
    p = sub.add_parser("call"); p.add_argument("method"); p.add_argument("params", nargs="?", default="{}")
    p.add_argument("--stepup", action="store_true"); p.set_defaults(fn=cmd_call)
    p = sub.add_parser("find"); p.add_argument("key"); p.add_argument("value"); p.set_defaults(fn=cmd_find)
    p = sub.add_parser("contains"); p.add_argument("text", nargs="+"); p.set_defaults(fn=cmd_contains)
    p = sub.add_parser("pick-proposal"); p.add_argument("label"); p.set_defaults(fn=cmd_pick_proposal)
    p = sub.add_parser("employee"); p.add_argument("label"); p.add_argument("--all", action="store_true"); p.set_defaults(fn=cmd_employee)
    p = sub.add_parser("jget"); p.add_argument("path"); p.set_defaults(fn=cmd_jget)
    args = parser.parse_args()
    if args.data_home:
        os.environ["DUDUCLAW_HOME"] = args.data_home
        os.environ.pop("DUDUCLAW_PORT", None)
    started = time.monotonic()
    try:
        result = args.fn(args)
    except (Fail, probe.ProbeError, OSError, ValueError, KeyError) as error:
        print(json.dumps({"ok": False, "payload": None, "error": str(error)[:600],
                          "elapsed_ms": int((time.monotonic() - started) * 1000)}))
        return 0
    if result == "raw":
        return 0
    result = result or {}
    elapsed = result.pop("_elapsed_ms", int((time.monotonic() - started) * 1000))
    print(json.dumps({"ok": True, "payload": result.get("payload", result), "error": None, "elapsed_ms": elapsed}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
