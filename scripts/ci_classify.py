"""Conservatively classify PR diffs; execute this file from the trusted base."""

import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


FULL = dict(rust_mode="full", dashboard="run", discord_voice="true", app_compat="true")
SEMVER = re.compile(r"(?<![\d.])\d+\.\d+\.\d+(?![\d.])")
VERSION_FILES = {
    "Cargo.toml", "Cargo.lock", "pyproject.toml", "python/duduclaw/__init__.py",
    "README.md", "README.zh-TW.md", "README.ja.md", "scripts/install.sh",
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


def lock_versions_only(old: str, new: str) -> bool:
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
    return masked(old) == masked(new)


def text_at(sha: str, path: str) -> str:
    return git("show", f"{sha}:{path}")


def bump_only(base: str, head: str, changes: list[tuple[str, str]]) -> bool:
    has_version = False
    for status, path in changes:
        if status == "A" and re.fullmatch(r"distribution/release-delivery/v[^/]+\.(json|sig)", path):
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
            def content(text: str) -> list[str]:
                return [line for line in text.splitlines(keepends=True)
                        if line.strip() and not re.match(r"^## \[[^\]\n]+\]", line)]
            if content(old) != content(new):
                return False
        elif version_path(path):
            has_version = True
            if path.endswith("Cargo.lock"):
                if not lock_versions_only(old, new):
                    return False
            elif SEMVER.sub("<VERSION>", old) != SEMVER.sub("<VERSION>", new):
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
                        else "skip", discord_voice="false", app_compat="false")
        if bump_only(base, head, changes):
            return dict(rust_mode="check", dashboard="skip", discord_voice="false", app_compat="false")
        known_roots = ("crates/", ".github/", "scripts/", "vendor/", "src-tauri/", "web/")
        if any(not (p.startswith(known_roots) or docs_path(p) or version_path(p)) for p in paths):
            return FULL.copy()
        shared = any(p in SHARED_FEATURE_FILES or p.startswith("vendor/") for p in paths)
        voice = shared or any(p.startswith("crates/duduclaw-gateway/")
                              and ("discord" in p or "voice" in p) for p in paths)
        compat = shared or any(p.startswith(("crates/duduclaw-core/", "crates/duduclaw-cli/"))
                               and "compat" in p for p in paths)
        return dict(rust_mode="full", dashboard="run", discord_voice=str(voice).lower(),
                    app_compat=str(compat).lower())
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
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
