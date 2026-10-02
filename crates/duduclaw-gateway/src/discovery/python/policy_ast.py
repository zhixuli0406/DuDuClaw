"""Static AST gate for LLM-written policy code (first isolation layer).

Rejects: imports outside a small allowlist, dangerous builtins, any dunder
attribute access (except `__init__`, needed for `super().__init__`), and any
bare dunder name other than `__name__`-free class/def definitions.
Returns a reason string such as "ast:forbidden_import:os", or None if OK.
"""
from __future__ import annotations

import ast
from pathlib import Path

ALLOWED_IMPORTS = frozenset({
    "math", "statistics", "itertools", "functools", "collections", "dataclasses",
    "typing", "heapq", "bisect",
})
FORBIDDEN_NAMES = frozenset({
    "open", "eval", "exec", "__import__", "compile", "globals", "locals", "vars",
    "getattr", "setattr", "delattr", "input", "breakpoint", "memoryview",
    "__builtins__", "__loader__", "__spec__", "help", "exit", "quit",
    # explicitly named in the brief, in case they are reached without an import
    "os", "sys", "subprocess", "socket", "pathlib", "importlib", "ctypes", "builtins",
    "inspect", "types", "io", "resource", "signal", "threading", "multiprocessing",
    "get_type_hints", "ForwardRef", "evaluate_forward_ref", "singledispatch", "singledispatchmethod",
})
FORBIDDEN_ATTRIBUTES = frozenset({
    "gi_frame", "gi_code", "gi_yieldfrom", "ag_frame", "ag_code", "cr_frame", "cr_code",
    "cr_await", "f_back", "f_globals", "f_locals", "f_builtins", "f_code", "tb_frame", "tb_next",
})
ALLOWED_DUNDER_ATTRS = frozenset({"__init__"})
MAX_SOURCE_BYTES = 256 * 1024


def check_source(source: str) -> str | None:
    if len(source.encode("utf-8")) > MAX_SOURCE_BYTES:
        return "ast:source_too_large"
    try:
        tree = ast.parse(source)
    except SyntaxError as e:
        return f"ast:syntax_error:{e.lineno}"
    has_policy = False
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".")[0]
                if root not in ALLOWED_IMPORTS:
                    return f"ast:forbidden_import:{root}"
        elif isinstance(node, ast.ImportFrom):
            if node.level and node.level > 0:
                return "ast:relative_import"
            root = (node.module or "").split(".")[0]
            if root not in ALLOWED_IMPORTS:
                return f"ast:forbidden_import:{root}"
            for alias in node.names:
                if alias.name in FORBIDDEN_NAMES:
                    return f"ast:forbidden_name:{alias.name}"
                if alias.name.startswith("_") or alias.name == "*":
                    return f"ast:private_import:{alias.name}"
        elif isinstance(node, ast.Name):
            if node.id in FORBIDDEN_NAMES:
                return f"ast:forbidden_name:{node.id}"
            if node.id.startswith("__") and node.id.endswith("__"):
                return f"ast:dunder_name:{node.id}"
        elif isinstance(node, ast.Attribute):
            if node.attr.startswith("__") and node.attr not in ALLOWED_DUNDER_ATTRS:
                return f"ast:dunder_attribute:{node.attr}"
            if node.attr in FORBIDDEN_NAMES:
                return f"ast:forbidden_attribute:{node.attr}"
            if node.attr in FORBIDDEN_ATTRIBUTES:
                return f"ast:frame_attribute:{node.attr}"
            if node.attr.startswith("_") and node.attr not in ALLOWED_DUNDER_ATTRS:
                return f"ast:private_attribute:{node.attr}"
        elif isinstance(node, (ast.Global, ast.Nonlocal)) and any(n.startswith("__") for n in node.names):
            return "ast:dunder_global"
        elif isinstance(node, ast.ClassDef) and node.name == "OptimalPolicy":
            has_policy = True
    if not has_policy:
        return "ast:missing_OptimalPolicy"
    return None


def check_file(path: Path) -> str | None:
    try:
        return check_source(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError) as e:
        return f"ast:unreadable:{type(e).__name__}"
