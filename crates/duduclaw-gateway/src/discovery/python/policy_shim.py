"""Trusted shim: loads an untrusted method.py and speaks the SPEC §7 protocol.

Runs inside the sandboxed subprocess. Imports stdlib only. The protocol uses a
private duplicate of fd 1; `sys.stdout` is redirected to stderr so a stray
print() in the policy cannot corrupt the protocol stream.

argv: policy_shim.py <method.py>
Exit codes: 0 after sending `done`/`plan`; 3 policy exception; 4 load failure.
"""
import json
import os
import sys
import traceback
import types
import builtins

# Never import anything from the harness directory.
if sys.path and sys.path[0] == os.path.dirname(os.path.abspath(__file__)):
    sys.path.pop(0)

_PROTO_OUT = os.fdopen(os.dup(1), "w", encoding="utf-8", buffering=1)
_PROTO_IN = sys.stdin
sys.stdout = sys.stderr


class Record(dict):
    """dict with attribute access (Observation / CellMeta)."""

    def __getattr__(self, name):
        try:
            return self[name]
        except KeyError as e:
            raise AttributeError(name) from e


class IllegalBatchError(Exception):
    pass


class MetaError(Exception):
    pass


class EpisodeTerminated(BaseException):
    """Not an Exception subclass, so `except Exception` in a policy won't swallow it."""


def _send(obj):
    _PROTO_OUT.write(json.dumps(obj, separators=(",", ":")) + "\n")
    _PROTO_OUT.flush()


def _recv():
    line = _PROTO_IN.readline()
    if not line:
        raise EpisodeTerminated("host closed")
    return json.loads(line)


class Question:
    def __init__(self, baseline_score, max_parallelism):
        self.baseline_score = baseline_score
        self.max_parallelism = max_parallelism

    def _call(self, req):
        _send(req)
        resp = _recv()
        if resp.get("ok"):
            return resp
        err = resp.get("error")
        if err == "illegal_batch":
            raise IllegalBatchError(resp.get("detail", ""))
        if err == "terminated":
            raise EpisodeTerminated("terminated")
        if err == "meta_unavailable":
            raise MetaError(resp.get("detail", ""))
        raise RuntimeError(f"host error: {err}")

    def reset(self):
        return None

    def observed(self):
        return {k: Record(v) for k, v in self._call({"op": "observed"})["observed"].items()}

    def legal_actions(self):
        return list(self._call({"op": "legal_actions"})["cells"])

    def legal_roots(self):
        return list(self._call({"op": "legal_roots"})["cells"])

    def opened_branches(self):
        return list(self._call({"op": "opened_branches"})["branches"])

    def meta(self, cell_id):
        return Record(self._call({"op": "meta", "cell_id": cell_id})["meta"])

    def probe_batch(self, cells, on_reveal=None):
        obs = [Record(o) for o in self._call({"op": "probe_batch", "cells": list(cells)})["observations"]]
        if on_reveal is not None:
            for o in obs:
                on_reveal(o)
        return obs


def _load_policy_class(path):
    with open(path, "r", encoding="utf-8") as f:
        src = f.read()
    reason = check_source(src)
    if reason:
        raise RuntimeError(reason)
    module = types.ModuleType("policy_method")
    module.__file__ = "method.py"
    sys.modules[module.__name__] = module
    ns = module.__dict__
    # A static gate is only one layer: permitted stdlib modules can export
    # other modules (dataclasses.inspect, typing.sys), and annotation helpers
    # can evaluate strings. Expose safe public exports and restricted builtins.
    safe_builtins = {k: v for k, v in vars(builtins).items() if k not in FORBIDDEN_NAMES}
    safe_builtins["__import__"] = _safe_import
    ns["__builtins__"] = safe_builtins
    exec(compile(src, "method.py", "exec", dont_inherit=True), ns)
    cls = ns.get("OptimalPolicy")
    if cls is None:
        raise RuntimeError("method.py defines no OptimalPolicy")
    return cls


def _safe_import(name, globals=None, locals=None, fromlist=(), level=0):
    if level or name.split(".")[0] not in ALLOWED_IMPORTS:
        raise ImportError("policy import refused")
    original = builtins.__import__(name, globals, locals, fromlist, level)
    proxy = types.ModuleType(original.__name__)
    for key, value in vars(original).items():
        if key.startswith("_") or key in FORBIDDEN_NAMES or isinstance(value, types.ModuleType):
            continue
        setattr(proxy, key, value)
    return proxy


def main():
    if len(sys.argv) != 2:
        sys.stderr.write("usage: policy_shim.py <method.py>\n")
        return 4
    init = _recv()
    try:
        cls = _load_policy_class(sys.argv[1])
        policy = cls(dict(init.get("config") or {}))
    except Exception:
        traceback.print_exc()
        return 4
    mode = init.get("mode")
    try:
        if mode == "default_beta":
            beta = getattr(policy, "beta", None)
            if isinstance(beta, bool) or not isinstance(beta, (int, float)):
                raise ValueError("policy must expose a scalar default beta")
            _send({"op": "default_beta", "default_beta": float(beta)})
            return 0
        if mode == "plan_grid":
            plan = policy.plan_grid(dict(init.get("context") or {}))
            if not isinstance(plan, dict):
                plan = {k: getattr(plan, k, None) for k in ("branch_count", "refine_count", "reason")}
            beta = getattr(policy, "beta", None)
            out = {"op": "plan", "branch_count": plan.get("branch_count"),
                   "refine_count": plan.get("refine_count"), "reason": str(plan.get("reason", ""))[:500]}
            if isinstance(beta, (int, float)) and not isinstance(beta, bool):
                out["default_beta"] = float(beta)
            _send(out)
            return 0
        if mode == "solve":
            q = Question(init.get("baseline_score"), init.get("max_parallelism"))
            try:
                policy.solve(q)
            except EpisodeTerminated:
                pass
            _send({"op": "done"})
            return 0
        sys.stderr.write(f"unknown mode {mode!r}\n")
        return 4
    except Exception:
        traceback.print_exc()
        return 3


if __name__ == "__main__":
    sys.exit(main())
