"""Conservatively classify PR diffs; execute this file from the trusted base."""

import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


FULL = dict(rust_mode="full", dashboard="run", app_compat="true")
VERSION_FILES = {
    "Cargo.toml", "Cargo.lock", "pyproject.toml", "python/duduclaw/__init__.py",
    "scripts/install.sh",
    "scripts/install.ps1", "src-tauri/tauri.conf.json", "src-tauri/Cargo.lock",
    "distribution/registries/mcp/server.json",
}
SHARED_FEATURE_FILES = {"Cargo.toml", "Cargo.lock", "crates/duduclaw-gateway/Cargo.toml",
                        ".github/workflows/ci.yml"}


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True,
                          encoding="utf-8").stdout


def commit(sha: str) -> str:
    if not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", sha):
        raise ValueError("expected a full commit SHA")
    return git("rev-parse", "--verify", f"{sha}^{{commit}}").strip()


def docs_path(path: str) -> bool:
    return ((path.startswith("docs/") and path != "docs/README.md"
             and not path.startswith("docs/assets/"))
            or path.startswith(("wiki/", "marketing/", "team/"))
            or ("/" not in path and path.endswith(".md")))


def version_path(path: str) -> bool:
    return (path in VERSION_FILES
            or re.fullmatch(r"crates/[^/]+/Cargo\.toml|npm/[^/]+/package\.json", path) is not None)


def lock_versions_only(old: str, new: str, old_version: str | None = None,
                       new_version: str | None = None) -> bool:
    """Mask only workspace package version fields; preserve every other byte."""
    def masked(text: str) -> str:
        tomllib.loads(text)
        blocks = text.split("[[package]]")
        for index, block in enumerate(blocks[1:], 1):
            if (re.search(r'^name = "duduclaw[^"\n]*"$', block, re.MULTILINE)
                    and not re.search(r"^source = ", block, re.MULTILINE)):
                blocks[index] = re.sub(r'^version = "\d+\.\d+\.\d+"$',
                                       'version = "<VERSION>"', block, flags=re.MULTILINE)
        return "[[package]]".join(blocks)
    if masked(old) != masked(new):
        return False
    if old_version is not None:
        before = tomllib.loads(old).get("package", [])
        after = tomllib.loads(new).get("package", [])
        for left, right in zip(before, after):
            if left.get("version") != right.get("version"):
                if (left.get("version"), right.get("version")) != (old_version, new_version):
                    return False
    return True


def text_at(sha: str, path: str) -> str:
    return git("show", f"{sha}:{path}")


def bump_only(base: str, head: str, changes: list[tuple[str, str]]) -> bool:
    def workspace_version(sha: str) -> str:
        version = tomllib.loads(text_at(sha, "Cargo.toml"))["workspace"]["package"]["version"]
        if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
            raise ValueError("expected a numeric workspace version")
        return version

    old_version, new_version = workspace_version(base), workspace_version(head)
    if tuple(map(int, new_version.split("."))) <= tuple(map(int, old_version.split("."))):
        return False
    delivery = {f"distribution/release-delivery/v{new_version}.{ext}" for ext in ("json", "sig")}
    if {path for status, path in changes if status == "A"} != delivery:
        return False
    has_version = False
    for status, path in changes:
        if status == "A" and path in delivery:
            continue
        if status != "M":
            return False
        # Mode changes (including symlink replacement) are not version bumps.
        before = git("ls-tree", base, "--", path).split()[0]
        after = git("ls-tree", head, "--", path).split()[0]
        if before != after or before not in {"100644", "100755"}:
            return False
        old, new = text_at(base, path), text_at(head, path)
        if path == "CHANGELOG.md":
            header = re.compile(r"^## \[" + re.escape(new_version) + r"\](?:[ \t].*)?$", re.MULTILINE)
            if not header.search(new) or header.search(old):
                return False
            def content(text: str) -> list[str]:
                return [line for line in text.splitlines(keepends=True)
                        if line.strip() and not re.match(r"^## \[[^\]\n]+\]", line)]
            if content(old) != content(new):
                return False
        elif path in {"README.md", "README.zh-TW.md", "README.ja.md"}:
            continue
        elif version_path(path):
            has_version = True
            if path.endswith("Cargo.lock"):
                if not lock_versions_only(old, new, old_version, new_version):
                    return False
            else:
                old_lines, new_lines = old.splitlines(keepends=True), new.splitlines(keepends=True)
                if len(old_lines) != len(new_lines) or any(
                        left != right and left.replace(old_version, new_version) != right
                        for left, right in zip(old_lines, new_lines)):
                    return False
        else:
            return False
    return has_version and ("M", "CHANGELOG.md") in changes


def classify(event: str, base: str, head: str) -> dict[str, str]:
    if event != "pull_request":
        return FULL.copy()
    try:
        base, head = commit(base), commit(head)
        base = git("merge-base", base, head).strip()
        fields = git("diff", "--no-ext-diff", "--no-textconv", "--no-renames",
                     "--name-status", "-z", base, head).split("\0")
        if fields.pop() != "" or len(fields) % 2:
            return FULL.copy()
        changes = list(zip(fields[::2], fields[1::2]))
        if not changes or any(status not in {"A", "M", "D"} for status, _ in changes):
            return FULL.copy()
        paths = [path for _, path in changes]
        if all(docs_path(p) or p.startswith("web/") for p in paths):
            return dict(rust_mode="skip", dashboard="run" if any(p.startswith("web/") for p in paths)
                        else "skip", app_compat="false")
        if bump_only(base, head, changes):
            return dict(rust_mode="check", dashboard="skip", app_compat="false")
        known_roots = ("crates/", ".github/", "scripts/", "vendor/", "src-tauri/", "web/")
        if any(not (p.startswith(known_roots) or docs_path(p) or version_path(p)) for p in paths):
            return FULL.copy()
        shared = any(p in SHARED_FEATURE_FILES or p.startswith("vendor/") for p in paths)
        compat = shared or any(p.startswith(("crates/duduclaw-core/", "crates/duduclaw-cli/"))
                               and "compat" in p for p in paths)
        return dict(rust_mode="full", dashboard="run", app_compat=str(compat).lower())
    except (OSError, ValueError, IndexError, KeyError, TypeError, subprocess.SubprocessError):
        return FULL.copy()


def write_outputs(outputs: dict[str, str]) -> None:
    body = "".join(f"{key}={value}\n" for key, value in outputs.items())
    if output := os.environ.get("GITHUB_OUTPUT"):
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write(body)
    else:
        print(body, end="")


if __name__ == "__main__":
    write_outputs(classify(os.environ.get("GITHUB_EVENT_NAME", ""),
                           sys.argv[1] if len(sys.argv) > 1 else os.environ.get("BASE_SHA", ""),
                           sys.argv[2] if len(sys.argv) > 2 else os.environ.get("HEAD_SHA", "")))
