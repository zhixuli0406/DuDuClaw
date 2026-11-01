"""Login helper contract with fake CDP and an isolated-world DOM fixture.

No Docker/browser or third-party Python package is needed. Node executes the
actual helper JavaScript against the small DOM fixture (skipped if absent).
"""

from collections import deque
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import unittest
from unittest.mock import patch
from urllib.parse import urlencode


path = Path(__file__).with_name("duduclaw-fill-login")
loader = importlib.machinery.SourceFileLoader("fill_login", str(path))
spec = importlib.util.spec_from_loader(loader.name, loader)
login = importlib.util.module_from_spec(spec)
loader.exec_module(login)

SECRET = "never-print-this-password-\"\\\n"
CREDENTIAL = {"origin": "https://example.test", "login_path": "/login", "username": "user@example.test", "password": SECRET}

# This fixture supplies native prototype methods to the actual JS emitted by
# the helper. It records writes/submissions, including page event mutations.
DOM_FIXTURE = r"""
const vm = require('vm');
const readline = require('readline');
let config, context, state;
function setup(options) {
  config = options;
  state = {writes: [], submitted: 0, inputs: [], iframes: [], forms: []};
  class Element {
    constructor(attrs = {}) { this.attrs = attrs; this.isConnected = true; }
    getAttribute(key) { return this.attrs[key] ?? null; }
    getBoundingClientRect() { return {width: this.hidden ? 0 : 100, height: 20}; }
  }
  class HTMLElement extends Element { click() { this.form.requestSubmit(); } }
  class HTMLInputElement extends HTMLElement {
    constructor(type, name, form) {
      super({name}); this.type = type; this.form = form; this._value = '';
    }
    get value() { return this._value; }
    set value(value) {
      this._value = value;
      state.writes.push({type: this.type, value});
    }
    dispatchEvent(event) {
      if (config.refill && this._value === '') this._value = 'page-refilled';
      if (config.mutateAction && this._value !== '') this.form.attrs.action = 'https://evil.test/steal';
      if (config.mutateMethod && this._value !== '' && (!config.passwordOnly || this.type === 'password')) this.form.attrs.method = 'get';
      if (config.mutateSubmitter && this._value !== '') state.button.attrs.formmethod = 'get';
      if (config.mutatePath && this._value !== '') context.location.pathname = '/other';
      return true;
    }
  }
  class HTMLFormElement extends HTMLElement {
    requestSubmit() {
      state.submitted++;
      if (config.submitMutatesMethod) this.attrs.method = 'get';
      if (config.twoStep && state.submitted === 1) {
        for (const input of state.inputs) input.isConnected = false;
        state.inputs = [new HTMLInputElement('password', 'password', this)];
        context.location.pathname = config.stepPath || '/login';
        if (config.stepOrigin) context.location.origin = config.stepOrigin;
        if (config.hiddenPassword) state.inputs[0].hidden = true;
      } else if (config.outcome === 'success') {
        for (const input of state.inputs) input.isConnected = false;
        state.inputs = [];
      } else if (config.outcome === 'otp') {
        const otp = new HTMLInputElement('text', 'otp', this);
        otp.attrs.autocomplete = 'one-time-code';
        state.inputs.push(otp);
      } else if (config.outcome === 'captcha') {
        state.iframes.push(new Element({src: 'https://captcha.test/challenge'}));
      } else if (config.outcome === 'cross_origin') {
        context.location.origin = 'https://other.test';
      }
    }
  }
  const form = new HTMLFormElement({action: config.action || '/login', target: config.target || '', method: config.method ?? 'post'});
  if (config.omitMethod) delete form.attrs.method;
  state.forms = [form];
  const user = new HTMLInputElement('email', 'email', form);
  state.inputs = [user];
  if (!config.twoStep) state.inputs.push(new HTMLInputElement('password', 'password', form));
  const button = new HTMLElement({});
  if (config.formaction !== undefined) button.attrs.formaction = config.formaction;
  if (config.formmethod !== undefined) button.attrs.formmethod = config.formmethod;
  state.button = button;
  button.type = 'submit'; button.form = form;
  if (config.fallbackSubmit) HTMLFormElement.prototype.requestSubmit = undefined;
  const document = {
    baseURI: 'https://example.test/login', visibilityState: 'visible',
    querySelectorAll(selector) {
      if (selector === 'input') return state.inputs;
      if (selector === 'iframe') return state.iframes;
      if (selector === 'button, input') return [button, ...state.inputs];
      throw Error('unexpected selector');
    },
    querySelector() { return null; },
  };
  context = vm.createContext({Element, HTMLElement, HTMLInputElement, HTMLFormElement,
    Event: class Event {constructor(type) {this.type = type;}}, document,
    getComputedStyle: () => ({display: 'block', visibility: 'visible', opacity: '1'}),
    URL, location: {origin: config.origin || 'https://example.test', pathname: config.path || '/login', href: 'https://example.test/login'}});
}
readline.createInterface({input: process.stdin}).on('line', line => {
  const message = JSON.parse(line);
  try {
    if (message.setup) { setup(message.setup); process.stdout.write('{}\n'); }
    else if (message.inspect) process.stdout.write(JSON.stringify({
      writes: state.writes, submitted: state.submitted, values: state.inputs.map(el => el.value)
    }) + '\n');
    else process.stdout.write(JSON.stringify({value: vm.runInContext(message.expression, context)}) + '\n');
  } catch (_) { process.stdout.write('{"exception":true}\n'); }
});
"""


class FakeCDP:
    def __init__(self, **config):
        self.process = subprocess.Popen(
            [shutil.which("node"), "-e", DOM_FIXTURE], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.request({"setup": config})
        self.evaluations = []
        self.commands = []
        self.queue = deque()
        self.next_id = 0
        self.config = config
        self.intercepted = False
        self.polls = 0

    def send_command(self, method, params=None):
        self.next_id += 1
        command_id = self.next_id
        self.commands.append((method, params))
        result = self.call(method, params)
        if method == "Runtime.evaluate" and not self.intercepted and self.config.get("requests"):
            self.intercepted = True
            self.enqueue_request(0)
        if method == "Page.getFrameTree" and self.intercepted and self.config.get("delayedRedirect"):
            self.polls += 1
            if self.polls == 5:
                self.enqueue_request(1)
        if method == "Page.getFrameTree" and self.intercepted and self.config.get("delayedSuccess"):
            self.polls += 1
            if self.polls == 5:
                self.queue.append({"method": "Network.loadingFinished", "params": {"requestId": "login-network"}})
        if method == "Fetch.continueRequest":
            index = int(params["requestId"].split("-")[1]) + 1
            if index < len(self.config.get("requests", [])) and not self.config.get("delayedRedirect"):
                # A redirect is discovered only after the previous hop was
                # continued; the event pump must inspect it before it can run.
                self.queue.append({"method": "Network.responseReceived", "params": {
                    "requestId": f"network-{index-1}",
                    "response": {"status": self.config.get("redirectStatus", 307)},
                }})
                self.enqueue_request(index)
            elif not self.config.get("stuckRequest") and not self.config.get("delayedRedirect") and not self.config.get("delayedSuccess"):
                self.queue.append({"method": "Network.loadingFinished", "params": {"requestId": "login-network"}})
        if method == "Fetch.failRequest":
            self.queue.append({"method": "Network.loadingFailed", "params": {"requestId": "login-network"}})
        if self.config.get("failCommand") == method:
            self.queue.append({"id": command_id, "error": {"message": "fixture failure"}})
        else:
            self.queue.append({"id": command_id, "result": result})
        return command_id

    def enqueue_request(self, index):
        if index == 0:
            self.queue.append({"method": "Network.requestWillBeSent", "params": {"requestId": "login-network"}})
        self.queue.append({"method": "Fetch.requestPaused", "params": {
            "requestId": f"hop-{index}", "request": {
                "url": self.config["requests"][index], "method": self.config.get("requestMethod", "POST"), "postData": SECRET,
            }, "networkId": None if self.config.get("missingNetworkId") else "login-network", "redirectedRequestId": f"hop-{index-1}" if index else None,
        }})

    def recv_json(self):
        return self.queue.popleft()

    def request(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()
        return json.loads(self.process.stdout.readline())

    def call(self, method, params=None):
        if method == "Page.getFrameTree":
            return {"frameTree": {"frame": {"id": "main"}}}
        if method == "Page.createIsolatedWorld":
            if params["grantUniveralAccess"] is not False:
                raise AssertionError("universal access forbidden")
            return {"executionContextId": 7}
        if method == "Runtime.evaluate":
            assert params["contextId"] == 7 and params["silent"]
            self.evaluations.append(params["expression"])
            result = self.request({"expression": params["expression"]})
            return {"exceptionDetails": {}} if result.get("exception") else {"result": result}
        if method in ("Network.enable", "Network.setBypassServiceWorker", "Fetch.enable", "Fetch.disable",
                      "Fetch.continueRequest", "Fetch.failRequest", "Browser.close"):
            return {}
        raise AssertionError(method)

    def inspect(self):
        return self.request({"inspect": True})

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=2)
        self.process.stdout.close()
        self.process.stderr.close()


@unittest.skipUnless(shutil.which("node"), "Node is needed to execute the isolated-world fixture")
class FillLoginTests(unittest.TestCase):
    def run_login(self, credential=None, **config):
        ws = FakeCDP(**config)
        self.addCleanup(ws.close)
        ticks = iter(i / 10 for i in range(1000))
        with patch.object(login.time, "sleep"), patch.object(login.time, "monotonic", side_effect=ticks):
            status = login.fill_login(ws, credential or CREDENTIAL)
        return status, ws.inspect(), ws

    def test_origin_mismatch_does_not_fill(self):
        status, state, _ = self.run_login(origin="https://other.test")
        self.assertEqual(status, "origin_mismatch")
        self.assertEqual(state["writes"], [])
        self.assertEqual(state["submitted"], 0)

    def test_cross_origin_action_does_not_fill(self):
        status, state, _ = self.run_login(action="https://evil.test/login")
        self.assertEqual(status, "form_origin_mismatch")
        self.assertEqual(state["writes"], [])
        self.assertEqual(state["submitted"], 0)

    def test_two_step_path_fills_then_submits(self):
        status, state, _ = self.run_login(twoStep=True, outcome="success")
        self.assertEqual(status, "logged_in")
        self.assertEqual(state["submitted"], 2)
        self.assertEqual(state["writes"][:2], [
            {"type": "email", "value": CREDENTIAL["username"]},
            {"type": "password", "value": SECRET},
        ])

    def test_failed_path_clears_username_and_password(self):
        status, state, _ = self.run_login(outcome="failed")
        self.assertEqual(status, "failed")
        self.assertEqual(state["values"], ["", ""])
        self.assertEqual(state["submitted"], 1)

    def test_takeover_paths_clear_fields(self):
        for outcome in ("otp", "captcha"):
            with self.subTest(outcome=outcome):
                status, state, _ = self.run_login(outcome=outcome)
                self.assertEqual(status, "needs_takeover")
                self.assertTrue(all(value == "" for value in state["values"]))

    def test_failed_clear_is_not_reported_as_clean(self):
        status, _, _ = self.run_login(outcome="failed", refill=True)
        self.assertEqual(status, "failed_uncleared")

    def test_action_mutation_from_input_handler_stops_before_password(self):
        status, state, _ = self.run_login(mutateAction=True)
        self.assertEqual(status, "form_origin_mismatch")
        self.assertFalse(any(write["value"] == SECRET for write in state["writes"]))
        self.assertEqual(state["submitted"], 0)
        self.assertEqual(state["values"], ["", ""])

    def test_cross_origin_after_submit_fails_closed(self):
        status, _, _ = self.run_login(outcome="cross_origin")
        self.assertEqual(status, "failed_uncleared")

    def test_new_window_target_does_not_fill(self):
        status, state, _ = self.run_login(target="_blank")
        self.assertEqual(status, "form_origin_mismatch")
        self.assertEqual(state["writes"], [])

    def test_fallback_submitter_cross_origin_does_not_fill(self):
        status, state, _ = self.run_login(fallbackSubmit=True, formaction="https://evil.test/login")
        self.assertEqual(status, "form_origin_mismatch")
        self.assertEqual(state["submitted"], 0)
        self.assertEqual(state["writes"], [])
        self.assertEqual(state["values"], ["", ""])

    def test_same_origin_other_path_never_fills_even_if_listed_for_step_two(self):
        status, state, _ = self.run_login(path="/password", credential={**CREDENTIAL, "allowed_paths": ["/password"]})
        self.assertEqual(status, "failed")
        self.assertEqual(state["writes"], [])

    def test_two_step_requires_explicit_path_and_visible_password(self):
        status, state, _ = self.run_login(twoStep=True, stepPath="/password", outcome="success")
        self.assertEqual(status, "failed")
        self.assertFalse(any(w["value"] == SECRET for w in state["writes"]))
        status, state, _ = self.run_login(twoStep=True, stepPath="/password", outcome="success",
                                        credential={**CREDENTIAL, "allowed_paths": ["/password"]})
        self.assertEqual(status, "logged_in")
        self.assertEqual(state["submitted"], 2)
        status, state, _ = self.run_login(twoStep=True, stepPath="/password", hiddenPassword=True,
                                        credential={**CREDENTIAL, "allowed_paths": ["/password"]})
        self.assertEqual(status, "failed")
        self.assertFalse(any(w["value"] == SECRET for w in state["writes"]))

    def test_two_step_cross_origin_never_receives_password(self):
        status, state, _ = self.run_login(twoStep=True, stepOrigin="https://other.test")
        self.assertEqual(status, "failed_uncleared")
        self.assertFalse(any(w["value"] == SECRET for w in state["writes"]))

    def test_get_default_get_and_submitter_get_do_not_fill(self):
        for config in ({"omitMethod": True}, {"method": "get"}, {"method": ""}, {"method": "invalid"},
                       {"formmethod": "get"}, {"formmethod": ""},
                       {"fallbackSubmit": True, "formmethod": "get"}):
            with self.subTest(config=config):
                status, state, _ = self.run_login(**config)
                self.assertEqual(status, "failed")
                self.assertEqual(state["writes"], [])
                self.assertEqual(state["submitted"], 0)

    def test_submitter_post_overrides_form_get(self):
        status, state, _ = self.run_login(method="get", formmethod="POST", outcome="success")
        self.assertEqual(status, "logged_in")
        self.assertEqual(state["submitted"], 1)

    def test_sync_method_or_path_mutation_clears_and_stops_submit(self):
        for config in ({"mutateMethod": True}, {"mutateMethod": True, "passwordOnly": True},
                       {"mutateSubmitter": True}, {"mutatePath": True}):
            with self.subTest(config=config):
                status, state, _ = self.run_login(**config)
                self.assertIn(status, ("failed", "form_origin_mismatch"))
                self.assertEqual(state["submitted"], 0)
                self.assertEqual(state["values"], ["", ""])
                if not config.get("passwordOnly"):
                    self.assertFalse(any(w["value"] == SECRET for w in state["writes"]))

    def test_fetch_checks_every_redirect_hop_and_bypasses_service_workers(self):
        for redirect in (307, 308):
            with self.subTest(redirect=redirect):
                status, state, ws = self.run_login(outcome="success", redirectStatus=redirect, requests=[
                    "https://example.test/login", "https://example.test/redirect", "https://other.test/steal"])
                self.assertEqual(status, "failed")
                self.assertIn(("Fetch.enable", {"patterns": [{"urlPattern": "*", "requestStage": "Request"}]}), ws.commands)
                self.assertIn(("Network.setBypassServiceWorker", {"bypass": True}), ws.commands)
                self.assertEqual([p["requestId"] for m, p in ws.commands if m == "Fetch.continueRequest"], ["hop-0", "hop-1"])
                self.assertEqual([p for m, p in ws.commands if m == "Fetch.failRequest"],
                                 [{"requestId": "hop-2", "errorReason": "BlockedByClient"}])
                self.assertTrue(all(v == "" for v in state["values"]))
                first_eval = next(i for i, (m, _) in enumerate(ws.commands) if m == "Runtime.evaluate")
                self.assertLess(ws.commands.index(("Network.setBypassServiceWorker", {"bypass": True})), first_eval)
                self.assertLess(next(i for i, (m, _) in enumerate(ws.commands) if m == "Fetch.enable"), first_eval)

    def test_delayed_redirect_remains_intercepted_after_password_disappears(self):
        for code in (307, 308):
            with self.subTest(code=code):
                status, _, ws = self.run_login(outcome="success", delayedRedirect=True, redirectStatus=code,
                    requests=["https://example.test/login", "https://other.test/steal"])
                self.assertEqual(status, "failed")
                fail = next(i for i, (m, _) in enumerate(ws.commands) if m == "Fetch.failRequest")
                disable = next(i for i, (m, _) in enumerate(ws.commands) if m == "Fetch.disable")
                self.assertLess(fail, disable)
                self.assertGreaterEqual(ws.polls, 5)

    def test_same_origin_post_completes_before_success_releases_interception(self):
        status, _, ws = self.run_login(outcome="success", delayedSuccess=True,
                                       requests=["https://example.test/login"])
        self.assertEqual(status, "logged_in")
        self.assertGreaterEqual(ws.polls, 5)
        self.assertTrue(any(m == "Fetch.continueRequest" for m, _ in ws.commands))
        self.assertTrue(any(m == "Fetch.disable" for m, _ in ws.commands))
        self.assertFalse(any(m in ("Fetch.failRequest", "Browser.close") for m, _ in ws.commands))

    def test_unobservable_request_completion_closes_browser(self):
        status, _, ws = self.run_login(outcome="success", missingNetworkId=True,
                                       requests=["https://example.test/login"])
        self.assertEqual(status, "failed_uncleared")
        self.assertTrue(any(m == "Browser.close" for m, _ in ws.commands))
        self.assertFalse(any(m == "Fetch.disable" for m, _ in ws.commands))

    def test_stuck_post_closes_browser_without_releasing_interception(self):
        status, _, ws = self.run_login(outcome="success", stuckRequest=True,
                                       requests=["https://example.test/login"])
        self.assertEqual(status, "failed_uncleared")
        self.assertTrue(any(m == "Browser.close" for m, _ in ws.commands))
        self.assertFalse(any(m == "Fetch.disable" for m, _ in ws.commands))
        self.assertFalse(any(m == "Network.setBypassServiceWorker" and p == {"bypass": False}
                             for m, p in ws.commands))

    def test_submit_handler_get_is_not_forwarded_with_credentials(self):
        status, state, ws = self.run_login(submitMutatesMethod=True, requestMethod="GET",
            requests=["https://example.test/login?" + urlencode({"email": CREDENTIAL["username"], "password": SECRET})])
        self.assertEqual(status, "failed")
        self.assertEqual(state["values"], ["", ""])
        self.assertTrue(any(m == "Fetch.failRequest" for m, _ in ws.commands))
        self.assertFalse(any(m == "Fetch.continueRequest" for m, _ in ws.commands))

    def test_fetch_checks_scheme_host_and_port_and_fails_setup_closed(self):
        for url in ("http://example.test/login", "https://example.test:8443/login", "data:text/plain,test"):
            with self.subTest(url=url):
                status, _, ws = self.run_login(outcome="success", requests=[url])
                self.assertEqual(status, "failed")
                self.assertTrue(any(m == "Fetch.failRequest" for m, _ in ws.commands))
        status, state, _ = self.run_login(failCommand="Network.setBypassServiceWorker")
        self.assertEqual(status, "failed")
        self.assertEqual(state["writes"], [])

    def test_password_never_appears_in_stdout_or_stderr(self):
        ws = FakeCDP(outcome="success")
        self.addCleanup(ws.close)
        out, err = io.StringIO(), io.StringIO()
        stdin = io.TextIOWrapper(io.BytesIO(json.dumps(CREDENTIAL).encode()))
        # main closes the socket; keep the fixture alive for test cleanup.
        with patch.object(login.sys, "argv", [str(path)]), patch.object(login.sys, "stdin", stdin), \
                patch.object(login, "WebSocket", return_value=ws), \
                patch.object(login, "visible_page", return_value="/devtools/page/test"), \
                patch.object(ws, "close"), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            login.main()
        self.assertEqual(out.getvalue(), "logged_in\n")
        self.assertEqual(err.getvalue(), "")
        self.assertNotIn(SECRET, out.getvalue())


class InputTests(unittest.TestCase):
    def test_input_requires_origin_path_and_values_only(self):
        self.assertEqual(login.read_credential(io.BytesIO(json.dumps(CREDENTIAL).encode())), CREDENTIAL)
        for origin in ("http://example.test", "https://example.test/login", "https://u:p@example.test",
                       "https://example.test:99999", "https://example.test#fragment"):
            with self.subTest(origin=origin), self.assertRaises(ValueError):
                login.read_credential(io.BytesIO(json.dumps({**CREDENTIAL, "origin": origin}).encode()))
        for extra in ({"origin": "http://localhost:8080"}, {"login_path": "//evil.test/login"},
                      {"login_path": "/login?query"}, {"allowed_paths": ["https://evil.test/"]},
                      {"allowed_paths": "/password"}, {"login_path": "/login#fragment"}):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                login.read_credential(io.BytesIO(json.dumps({**CREDENTIAL, **extra}).encode()))
        with self.assertRaises(ValueError):
            login.read_credential(io.BytesIO(json.dumps({k: v for k, v in CREDENTIAL.items() if k != "login_path"}).encode()))

    def test_arguments_are_rejected_without_echo(self):
        result = subprocess.run(["python3", str(path), SECRET], capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout, "failed\n")
        self.assertEqual(result.stderr, "")

    def test_bad_json_is_rejected_without_echo(self):
        result = subprocess.run(["python3", str(path)], input=SECRET, capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout, "failed\n")
        self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main()
