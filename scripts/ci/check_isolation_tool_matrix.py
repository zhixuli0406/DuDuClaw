#!/usr/bin/env python3
"""Reject duplicate tool rows and refusal drift in the isolation matrix."""
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[2]
MATRIX = Path("docs/architecture/native-linux-isolation-tools.md")
POLICY = Path("crates/duduclaw-mcp-boundary/src/tool_policy.rs")
DISPOSITION = Path("crates/duduclaw-cli/src/mcp_isolation_disposition.rs")
FORK = Path("crates/duduclaw-mcp-boundary/src/fork_policy.rs")
ROW = re.compile(r"^\|\s*`([a-z][a-z0-9_]*)`\s*\|\s*(.*?)\s*\|$")
EXECUTABLE = re.compile(
    r"\b(?:ExecutionBackend|ExecutionPlanFactory|WorkerPlanFactory|WorkerLauncher|WorkerSession|AuthorizedFile)\b"
    r"|Resource\s*→|(?:^|[;|])\s*(?:Host|Original gateway)\b"
)


def check(matrix, policy, disposition, fork):
    errors = []
    rows = {}
    for number, line in enumerate(matrix.splitlines(), 1):
        match = ROW.fullmatch(line)
        if not match:
            continue
        name, description = match.groups()
        if name in rows:
            errors.append(f"line {number}: duplicate tool {name} (including conflicting dispositions)")
        rows[name] = description
    names = set(re.findall(r'ToolPolicy\s*\{\s*name:\s*"([^"]+)"', policy))
    if not names:
        errors.append("policy inventory is empty or cannot be parsed")
    if set(rows) != names:
        errors.append(f"inventory mismatch: missing={sorted(names - rows.keys())}, extra={sorted(rows.keys() - names)}")
    host = set()
    for arm in re.finditer(r'((?:"[a-z0-9_]+"\s*\|?\s*)+)\s*=>\s*HostAuthority', disposition):
        host.update(re.findall(r'"([a-z0-9_]+)"', arm[1]))
    proposal = set()
    for arm in re.finditer(r'((?:"[a-z0-9_]+"\s*\|?\s*)+)\s*=>\s*ProposalRequired', disposition):
        proposal.update(re.findall(r'"([a-z0-9_]+)"', arm[1]))
    deferred = re.search(r'DEFERRED_TOOLS[^=]*=\s*\[(.*?)\];', fork, re.S)
    if not host or deferred is None:
        errors.append("refusal policy is empty or cannot be parsed")
        return errors
    deferred = set(re.findall(r'"([a-z0-9_]+)"', deferred[1]))
    for name, description in rows.items():
        expected = ("HostAuthority" if name in host else "ProposalRequired" if name in proposal
                    else "ForkNextRelease" if name in deferred else None)
        actual = re.findall(r"Refused:\s*([A-Za-z]+)", description)
        if actual != ([expected] if expected else []):
            errors.append(f"{name}: refusal contradicts code: expected={expected}, actual={actual}")
        if actual and (not description.startswith(f"Refused: {actual[0]}") or EXECUTABLE.search(description)):
            errors.append(f"{name}: refusal mixed with executable disposition")
    return errors


def check_repository(root=ROOT):
    return check(*(root.joinpath(path).read_text() for path in (MATRIX, POLICY, DISPOSITION, FORK)))


if __name__ == "__main__":
    failures = check_repository()
    if failures:
        print("\n".join(failures), file=sys.stderr)
        sys.exit(1)
    print("Isolation tool matrix: unique inventory and refusal policies match")
