#!/usr/bin/env python3
"""Fail when process effects change without an explicit inventory review.

This is a conservative lexical guard, not a Rust type checker or authorization
mechanism. It scans every crates/*/src/**/*.rs, including cfg(test) and platform
branches. Literal/comment contents do not count. Constructors, known platform
factories, PTY launches, raw exec/fork calls and process terminal methods are
counted per enclosing function. An entry records the current identity separately
from the intended isolation migration; a pending entry is not an exemption.
"""
from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = ROOT / "scripts/ci/privileged_effects.json"
RAW_LITERAL = re.compile(r'(?:br|cr|r)(#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'")
TOKEN = re.compile(r"[A-Za-z_][A-Za-z_0-9]*|::|->|[^\s]", re.ASCII)
FACTORIES = {"command_for", "async_command_for"}
RAW = {"fork", "vfork", "execve", "execveat", "execv", "execvp", "execl", "posix_spawn", "posix_spawnp", "clone", "clone3", "forkpty", "system", "popen"}


def mask_literals(source: str) -> str:
    """Mask nested comments and Rust literals, retaining offsets/newlines."""
    result = list(source)
    i = 0
    while i < len(source):
        end = None
        if source.startswith("//", i):
            end = source.find("\n", i)
            if end < 0:
                end = len(source)
        elif source.startswith("/*", i):
            depth, end = 1, i + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        else:
            raw = RAW_LITERAL.match(source, i) if source[i] in "rbc" else None
            if raw:
                closing = '"' + raw.group(1)
                pos = source.find(closing, i + len(raw.group()))
                end = len(source) if pos < 0 else pos + len(closing)
            elif source[i] == '"':
                end = i + 1
                while end < len(source):
                    if source[end] == "\\":
                        end += 2
                    elif source[end] == '"':
                        end += 1
                        break
                    else:
                        end += 1
            elif source[i] == "'":
                # A lifetime has no closing quote; a char literal does.
                char = CHAR_LITERAL.match(source, i)
                if char:
                    end = i + len(char.group())
        if end is None:
            i += 1
        else:
            for j in range(i, min(end, len(source))):
                if result[j] != "\n":
                    result[j] = " "
            i = end
    return "".join(result)


@dataclass
class Site:
    symbol: str
    line: int
    apis: dict[str, int]
    test_only: bool


def scan_text(source: str, path: str) -> list[Site]:
    if not any(word in source for word in ("execution_gate", "execution_blocking", "BackendChild", "PtyHandle", "Command", "spawn", "exec", "fork", "command_for", "openpty", "pty_system", "clone", "system", "popen")):
        return []
    masked = mask_literals(source)
    matches = list(TOKEN.finditer(masked))
    tokens = [m.group() for m in matches]
    # Resolve explicit import/type aliases. Recognizing them file-wide is
    # conservative: an alias reused in another scope is still inventoried.
    aliases = {"Command", "CommandBuilder", "PtyCommand"}
    for i, token in enumerate(tokens):
        if token in aliases and tokens[i + 1:i + 2] == ["as"] and i + 2 < len(tokens):
            aliases.add(tokens[i + 2])
        if token == "type" and i + 3 < len(tokens):
            end = next((j for j in range(i + 2, len(tokens)) if tokens[j] == ";"), i + 2)
            if any(t in aliases for t in tokens[i + 2:end]):
                aliases.add(tokens[i + 1])

    backend_aliases = {"execution_gate", "execution_blocking", "BackendChild", "PtyHandle"}
    for i, token in enumerate(tokens):
        if token in backend_aliases and tokens[i + 1:i + 2] == ["as"] and i + 2 < len(tokens):
            backend_aliases.add(tokens[i + 2])

    raw_aliases = {}
    raw_modules = {"libc", "unistd", "process"}
    for match in re.finditer(r"\buse\s+([^;]+);", masked):
        imported = [m.group() for m in TOKEN.finditer(match.group(1))]
        if not any(t in raw_modules for t in imported):
            continue
        for i, token in enumerate(imported):
            alias = imported[i + 2] if imported[i + 1:i + 2] == ["as"] else token
            if token in RAW:
                raw_aliases[alias] = token
            elif token in raw_modules and alias != token:
                raw_modules.add(alias)

    # Brace stack records fn/mod/impl context, with nested fn declarations
    # taking precedence. Signatures are included for Command-typed parameters.
    stack: list[tuple[str, str, int]] = []
    functions: list[dict] = []
    pending: tuple[str, str, int] | None = None
    active: list[int] = []
    brackets = 0
    parentheses = 0
    for i, token in enumerate(tokens):
        if token == "[":
            brackets += 1
        elif token == "]":
            brackets -= 1
        elif token == "(":
            parentheses += 1
        elif token == ")":
            parentheses -= 1
        if token == "fn" and i + 1 < len(tokens):
            pending = ("fn", tokens[i + 1], i)
        elif token == "mod" and i + 1 < len(tokens):
            pending = ("mod", tokens[i + 1], i)
        elif token == "impl" and pending is None:
            end = next((j for j in range(i + 1, len(tokens)) if tokens[j] == "{"), i + 1)
            pending = ("impl", "".join(tokens[i + 1:end]), i)
        elif token == ";" and brackets == 0 and parentheses == 0:
            pending = None
        elif token == "{":
            entry = pending or ("block", "", i)
            pending = None
            stack.append(entry)
            if entry[0] == "fn":
                context = [name for kind, name, _ in stack if kind != "block"]
                functions.append({"name": "::".join(context), "start": entry[2], "end": len(tokens), "depth": len(stack)})
                active.append(len(functions) - 1)
        elif token == "}":
            if stack:
                entry = stack.pop()
                if entry[0] == "fn" and active:
                    functions[active.pop()]["end"] = i + 1

    functions.append({"name": "<module>", "start": 0, "end": len(tokens), "depth": 0})
    sites: dict[str, Site] = {}
    # Multiple cfg implementations of a symbol share one count, intentionally.
    for function in functions:
        begin, end = function["start"], function["end"]
        nested = [(f["start"], f["end"]) for f in functions if f is not function and f["name"] != "<module>" and begin <= f["start"] < end and f["end"] <= end]
        owned = [i for i in range(begin, end) if not any(a <= i < b for a, b in nested)]
        process_hint = any(tokens[i] in aliases or tokens[i] in FACTORIES for i in owned)
        effects: Counter[str] = Counter()
        for i in owned:
            token = tokens[i]
            if token in aliases and tokens[i + 1:i + 4] == ["::", "new", "("]:
                # clap command trees do not create OS processes.
                if i >= 2 and tokens[i - 2:i] == ["clap", "::"]:
                    continue
                effects["command_constructor"] += 1
            elif token in aliases and tokens[i + 1:i + 2] == ["::"] and tokens[i + 2:i + 3] in (["spawn"], ["output"], ["status"], ["exec"]) and tokens[i + 3:i + 4] == ["("]:
                if i >= 2 and tokens[i - 2:i] == ["clap", "::"]:
                    continue
                effects["terminal_" + tokens[i + 2]] += 1
            elif token in backend_aliases and tokens[i + 1:i + 2] == ["::"] and tokens[i + 2:i + 3] in (["spawn"], ["spawn_class"], ["output"], ["status"], ["spawn_pty"]) and tokens[i + 3:i + 4] == ["("]:
                effects["backend_" + tokens[i + 2]] += 1
            elif token in FACTORIES and tokens[i + 1:i + 2] == ["("] and tokens[i - 1:i] != ["fn"]:
                effects["platform_command_factory"] += 1
            elif token in RAW and i >= 2 and tokens[i - 2] in raw_modules and tokens[i - 1] == "::":
                if tokens[i + 1:i + 2] == ["("]:
                    effects["raw_" + token] += 1
            elif token in raw_aliases and tokens[i + 1:i + 2] == ["("] and tokens[i - 1:i] != ["fn"]:
                effects["raw_" + raw_aliases[token]] += 1
            elif i > 0 and tokens[i - 1] == "." and tokens[i + 1:i + 2] == ["("]:
                if token in {"spawn", "spawn_authorized", "spawn_command", "exec"}:
                    effects["terminal_" + token] += 1
                elif token == "openpty":
                    effects["pty_openpty"] += 1
                elif process_hint and token in {"output", "status"}:
                    effects["terminal_" + token] += 1
            elif token in {"native_pty_system", "openpty"} and tokens[i + 1:i + 2] == ["("]:
                effects["pty_" + token] += 1
        if not effects:
            continue
        symbol = path + "::" + function["name"]
        test_only = any(Path(part).stem in {"tests", "test"} or Path(part).stem.endswith("_tests") or part.startswith("tests_") for part in Path(path).parts)
        test_only = test_only or any(part in {"tests", "test", "test_support"} or part.endswith("_tests") or part.startswith("tests_") for part in function["name"].split("::"))
        line = source.count("\n", 0, matches[begin].start()) + 1
        if symbol in sites:
            existing = sites[symbol]
            existing.apis = dict(Counter(existing.apis) + effects)
        else:
            sites[symbol] = Site(symbol, line, dict(sorted(effects.items())), test_only)
    return sorted(sites.values(), key=lambda site: site.symbol)


def discover(root: Path) -> list[Site]:
    sites = []
    for path in sorted((root / "crates").glob("*/src/**/*.rs")):
        sites.extend(scan_text(path.read_text(), path.relative_to(root).as_posix()))
    return sites


def check(sites: list[Site], registry: dict) -> list[str]:
    errors = []
    if registry.get("schema_version") != 1:
        return ["unsupported inventory schema"]
    entries = registry.get("entries", [])
    indexed = {}
    identities = {"gateway", "employee", "parse_worker", "import_worker", "spawn_daemon", "trampoline", "egress", "updater", "downloader", "operator", "test", "host_service"}
    migrations = {"pending", "backend", "gateway_exception", "privileged_service", "isolated_child", "isolated_helper", "not_process", "test_only", "other_package"}
    for entry in entries:
        symbol = entry.get("symbol")
        if not symbol or symbol in indexed:
            errors.append(f"invalid or duplicate symbol: {symbol}")
            continue
        indexed[symbol] = entry
        if entry.get("identity") not in identities or entry.get("migration") not in migrations:
            errors.append(f"invalid classification: {symbol}")
        if not entry.get("rationale", "").strip() or not entry.get("effects"):
            errors.append(f"missing effect/rationale: {symbol}")
        if entry.get("migration") == "gateway_exception" and entry.get("identity") not in {"gateway", "operator"}:
            errors.append(f"invalid gateway exemption: {symbol}")
        if entry.get("migration") == "isolated_child" and (
            entry.get("identity") != "employee"
            or entry.get("apis") != {
                "crates/duduclaw-api-worker/src/linux.rs::Bridge::connect": {"command_constructor": 1, "terminal_spawn": 1},
                "crates/duduclaw-api-worker/src/native_main.rs::run": {"command_constructor": 1, "terminal_exec": 1},
            }.get(symbol)
        ):
            errors.append(f"invalid isolated runtime child: {symbol}")
        if entry.get("migration") == "privileged_service" and entry.get("identity") not in {
            "spawn_daemon", "trampoline", "updater", "downloader", "host_service",
        }:
            errors.append(f"invalid privileged service exemption: {symbol}")
        if entry.get("migration") == "isolated_helper" and (
            entry.get("identity") != "employee"
            or symbol not in {
                "crates/duduclaw-execution/src/program_helper.rs::execute_command",
                "crates/duduclaw-execution/src/recording_helper.rs::run_driver",
            }
        ):
            errors.append(f"invalid isolated helper exemption: {symbol}")
    observed = {site.symbol: site for site in sites}
    for symbol, site in observed.items():
        if symbol not in indexed:
            errors.append(f"unclassified: {symbol} (line {site.line}; {site.apis})")
        elif indexed[symbol].get("apis") != site.apis:
            errors.append(f"changed process effects: {symbol}: expected {indexed[symbol].get('apis')}, found {site.apis}")
    for symbol in indexed.keys() - observed.keys():
        errors.append(f"stale entry: {symbol}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--registry", type=Path, default=REGISTRY)
    parser.add_argument("--dump", action="store_true", help="Print observed symbols; never changes the reviewed registry")
    parser.add_argument("--require-migrated", action="store_true", help="Release gate: reject pending employee/gateway execution migrations")
    args = parser.parse_args()
    sites = discover(args.root)
    if args.dump:
        print(json.dumps([vars(site) for site in sites], indent=2))
        return 0
    registry = json.loads(args.registry.read_text())
    errors = check(sites, registry)
    pending = [e for e in registry["entries"] if e["migration"] == "pending"
        or (e["migration"] == "other_package" and "target_identity" in e)]
    if args.require_migrated:
        errors.extend(f"migration pending: {e['symbol']}" for e in pending)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Spawn inventory: {len(sites)} functions classified; {len(pending)} migrations pending (inventory coverage is not isolation acceptance).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
