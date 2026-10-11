#!/usr/bin/env python3
"""Verify a signed, public-safe release delivery assertion; never read private docs."""
import argparse
import base64
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ITEMS = {"it_manual", "user_manual", "product_site", "os_site", "company_site"}
SHA = re.compile(r"[0-9a-f]{40}")
HASH = re.compile(r"[0-9a-f]{64}")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")


def git(root, *args):
    # Repository-local replace objects and inherited Git overrides cannot change
    # the candidate whose source/delta was reviewed.
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return subprocess.check_output(
        ["git", "--no-replace-objects", "-C", str(root), *args], env=env,
        stderr=subprocess.DEVNULL)


def text(root, revision, path):
    return git(root, "show", f"{revision}:{path}").decode()


def delta(root, previous_tag, source):
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", previous_tag):
        raise ValueError("invalid previous release tag")
    # Require a real ancestor tag, never a caller-supplied arbitrary revision.
    git(root, "merge-base", "--is-ancestor", previous_tag, source)
    return hashlib.sha256(git(root, "diff", "--binary", "--no-ext-diff",
                              "--no-textconv", previous_tag, source, "--")).hexdigest()


def parse_unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate assertion field")
        result[key] = value
    return result


def verify_assertion(raw, signature, public_key, version, root, source):
    if len(raw) > 32768 or len(signature) > 16384:
        raise ValueError("assertion exceeds size limit")
    with tempfile.TemporaryDirectory(prefix="delivery-verify-") as temp:
        folder = Path(temp)
        (folder / "key.pem").write_text(public_key)
        (folder / "assertion.json").write_bytes(raw)
        (folder / "signature").write_bytes(base64.b64decode(signature, validate=True))
        result = subprocess.run(
            ["openssl", "dgst", "-sha256", "-verify", str(folder / "key.pem"),
             "-signature", str(folder / "signature"), str(folder / "assertion.json")],
            capture_output=True)
        if result.returncode:
            raise ValueError("delivery signature verification failed")
    data = json.loads(raw, object_pairs_hook=parse_unique)
    expected = {"schema", "version", "source_commit", "previous_tag", "delta_sha256",
                "private_review_sha256", "issued_at", "expires_at", "items"}
    if set(data) != expected or data["schema"] != 1:
        raise ValueError("unsupported assertion schema")
    if not VERSION.fullmatch(version) or data["version"] != version:
        raise ValueError("delivery version mismatch")
    if not SHA.fullmatch(data["source_commit"]) or data["source_commit"] != source:
        raise ValueError("delivery source mismatch")
    now = dt.datetime.now(dt.timezone.utc)
    issued = dt.datetime.fromisoformat(data["issued_at"])
    expiry = dt.datetime.fromisoformat(data["expires_at"])
    if issued.tzinfo is None or expiry.tzinfo is None or not issued <= now < expiry:
        raise ValueError("delivery assertion is expired or not yet valid")
    if expiry - issued > dt.timedelta(days=30):
        raise ValueError("delivery validity exceeds 30 days")
    if not HASH.fullmatch(data["private_review_sha256"]):
        raise ValueError("missing private review binding")
    if data["delta_sha256"] != delta(root, data["previous_tag"], source):
        raise ValueError("feature delta mismatch")
    if not isinstance(data["items"], dict) or set(data["items"]) != ITEMS:
        raise ValueError("both manuals and all three websites are required")
    for item in data["items"].values():
        if set(item) != {"status", "artifact_sha256", "evidence_sha256", "reason_sha256"}:
            raise ValueError("invalid delivery item schema")
        if item["status"] not in {"updated", "unchanged"}:
            raise ValueError("delivery item is incomplete")
        if not all(isinstance(item[k], str) and HASH.fullmatch(item[k])
                   for k in ("artifact_sha256", "evidence_sha256")):
            raise ValueError("delivery item lacks artifact or acceptance evidence")
        if item["status"] == "unchanged":
            if not isinstance(item["reason_sha256"], str) or not HASH.fullmatch(item["reason_sha256"]):
                raise ValueError("unchanged item lacks reviewed reason")
        elif item["reason_sha256"] is not None:
            raise ValueError("updated item must not have an unchanged reason")
    return data


def expected_bump(path, old, before, new, workspace_names):
    """Reproduce only release.sh's version transformations, not arbitrary edits."""
    if path == "CHANGELOG.md":
        # Caller checks the release header separately; all curated notes stay.
        return None
    if path.endswith("Cargo.lock"):
        blocks = before.split("[[package]]")
        for index, block in enumerate(blocks):
            name = re.search(r'^name = "([^"]+)"$', block, re.M)
            if name and name[1] in workspace_names:
                blocks[index] = re.sub(r'^version = "' + re.escape(old) + r'"$',
                                       f'version = "{new}"', block, count=1, flags=re.M)
        return "[[package]]".join(blocks)
    if path == "Cargo.toml" or re.fullmatch(r"crates/[^/]+/Cargo.toml", path) or path == "pyproject.toml":
        lines = before.splitlines(keepends=True)
        in_package = False
        done = False
        for index, line in enumerate(lines):
            if line.startswith("["):
                in_package = line.strip() in {"[package]", "[workspace.package]", "[project]"}
                done = False
            if in_package and not done and re.fullmatch(r'version = "[0-9]+\.[0-9]+\.[0-9]+"\s*', line):
                lines[index] = f'version = "{new}"\n'
                done = True
        return "".join(lines)
    patterns = {
        "python/duduclaw/__init__.py": r'(?m)^([ \t]*__version__ = ")[0-9]+\.[0-9]+\.[0-9]+(")',
        "scripts/install.sh": r'(?m)^(FALLBACK_VERSION=")[0-9]+\.[0-9]+\.[0-9]+(")',
        "scripts/install.ps1": r'(?m)^(\$FallbackVersion = ")[0-9]+\.[0-9]+\.[0-9]+(")',
    }
    if path in patterns:
        return re.sub(patterns[path], lambda m: m[1] + new + m[2], before)
    if path in {"README.md", "README.zh-TW.md", "README.ja.md"}:
        return re.sub(r'(badge/version-)[0-9]+\.[0-9]+\.[0-9]+(-blue)',
                      lambda m: m[1] + new + m[2], before)
    if re.fullmatch(r"npm/[^/]+/package.json", path) or path in {
            "src-tauri/tauri.conf.json", "distribution/registries/mcp/server.json"}:
        result = re.sub(r'("version"\s*:\s*")[0-9]+\.[0-9]+\.[0-9]+(")',
                        lambda m: m[1] + new + m[2], before)
        if path.startswith("npm/"):
            result = re.sub(r'("@duduclaw/[a-z0-9-]+"\s*:\s*")[0-9]+\.[0-9]+\.[0-9]+(")',
                            lambda m: m[1] + new + m[2], result)
        return result
    raise ValueError("release commit contains a non-version change")


def release_bump(root, revision):
    """Allow a reviewed bump, or a PR merge with the exact same tree."""
    parents = git(root, "rev-list", "--parents", "-n", "1", revision).decode().split()
    if len(parents) == 3:
        bump = parents[2]
        if git(root, "rev-parse", revision + "^{tree}") != git(root, "rev-parse", bump + "^{tree}"):
            raise ValueError("PR merge differs from reviewed release tree")
        parents = git(root, "rev-list", "--parents", "-n", "1", bump).decode().split()
    else:
        bump = revision
    if len(parents) != 2:
        raise ValueError("release bump must have exactly one reviewed source parent")
    return bump, parents[1]


def verify_release_commit(root, revision, source, version):
    revision, expected_source = release_bump(root, revision)
    parents = git(root, "rev-list", "--parents", "-n", "1", revision).decode().split()
    if expected_source != source:
        raise ValueError("release must be a single version-bump commit after reviewed source")
    old = re.search(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"', text(root, source, "Cargo.toml"), re.M)[1]
    current = re.search(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"', text(root, revision, "Cargo.toml"), re.M)[1]
    if current != version or current == old:
        raise ValueError("release manifest version mismatch")
    names = {"duduclaw"}
    files = git(root, "ls-tree", "-r", "--name-only", source).decode().splitlines()
    for path in files:
        if re.fullmatch(r"crates/[^/]+/Cargo.toml", path):
            name = re.search(r'^name = "([^"]+)"', text(root, source, path), re.M)
            if name:
                names.add(name[1])
    # A direct tag cannot skip a publish target and leave it at an older version.
    for path in files:
        if path == "CHANGELOG.md" or path.endswith("Cargo.lock"):
            continue
        try:
            before = text(root, source, path) if (
                path in {"Cargo.toml", "pyproject.toml", "python/duduclaw/__init__.py",
                         "scripts/install.sh", "scripts/install.ps1", "README.md", "README.zh-TW.md",
                         "README.ja.md", "src-tauri/tauri.conf.json", "distribution/registries/mcp/server.json"}
                or re.fullmatch(r"crates/[^/]+/Cargo.toml|npm/[^/]+/package.json", path)) else None
            if before is None:
                continue
            expected = expected_bump(path, old, before, version, names)
        except ValueError:
            continue
        if text(root, revision, path) != expected:
            raise ValueError("a publish manifest was not synchronized")
    # Only regular blobs retain their existing file modes. Added assertions
    # are plain non-executable files, not links/submodules or hidden mode edits.
    for row in git(root, "diff", "--raw", "--no-abbrev", source, revision).decode().splitlines():
        meta, path = row.split("\t", 1)
        old_mode, new_mode = meta.split()[:2]
        if path.startswith(f"distribution/release-delivery/v{version}."):
            if old_mode != ":000000" or new_mode != "100644":
                raise ValueError("assertion must be newly added regular file")
        elif old_mode[1:] != new_mode or new_mode not in {"100644", "100755"}:
            raise ValueError("release commit changes file types or modes")
    paths = git(root, "diff", "--name-only", source, revision).decode().splitlines()
    delivery = {f"distribution/release-delivery/v{version}.json",
                f"distribution/release-delivery/v{version}.sig"}
    if not delivery.issubset(paths):
        raise ValueError("release commit lacks committed delivery assertion")
    for path in paths:
        if path in delivery:
            continue
        before = text(root, source, path)
        after = text(root, revision, path)
        if path == "CHANGELOG.md":
            pattern = r'(## \[Unreleased\][^\n]*\n)\n## \[' + re.escape(version) + r'\] - [0-9]{4}-[0-9]{2}-[0-9]{2}[^\n]*\n'
            restored, count = re.subn(pattern, r'\1', after, count=1)
            if count != 1 or restored != before:
                raise ValueError("release changelog must preserve all reviewed feature notes")
        elif expected_bump(path, old, before, version, names) != after:
            raise ValueError("release commit changes more than version metadata")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--assertion", type=Path)
    parser.add_argument("--signature", type=Path)
    parser.add_argument("--public-key", type=Path)
    parser.add_argument("--release-ref", help="Validate a final release commit/tag instead of pre-bump HEAD")
    parser.add_argument("--require-main", action="store_true", help="Require released commit to be merged into origin/main")
    args = parser.parse_args()
    try:
        key = args.public_key.read_text() if args.public_key else os.environ.get("RELEASE_DELIVERY_PUBLIC_KEY_PEM", "")
        if not key:
            raise ValueError("RELEASE_DELIVERY_PUBLIC_KEY_PEM is not configured")
        if not VERSION.fullmatch(args.version):
            raise ValueError("invalid release version")
        if args.release_ref:
            if not re.fullmatch(r'(desktop-)?v' + re.escape(args.version), args.release_ref):
                raise ValueError("release ref must be the exact version tag")
            revision = git(args.root, "rev-parse", f"refs/tags/{args.release_ref}^{{commit}}").decode().strip()
            raw = git(args.root, "show", f"{revision}:distribution/release-delivery/v{args.version}.json")
            signature = git(args.root, "show", f"{revision}:distribution/release-delivery/v{args.version}.sig").strip()
            _, source = release_bump(args.root, revision)
            if args.require_main:
                git(args.root, "merge-base", "--is-ancestor", revision, "refs/remotes/origin/main")
        else:
            if not args.assertion or not args.signature:
                raise ValueError("a reviewed delivery assertion and signature are required before bumping")
            raw = args.assertion.read_bytes()
            signature = args.signature.read_bytes().strip()
            source = git(args.root, "rev-parse", "HEAD").decode().strip()
        verify_assertion(raw, signature, key, args.version, args.root, source)
        if args.release_ref:
            verify_release_commit(args.root, revision, source, args.version)
        print("Release delivery gate: PASS (two manuals and three websites independently reviewed)")
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError, json.JSONDecodeError):
        # Do not print external/private content, paths, diffs, or subprocess stderr.
        parser.exit(1, "Release delivery gate: BLOCKED; check configuration, version/source binding, signature and five reviewed deliverables.\n")


if __name__ == "__main__":
    main()
