#!/usr/bin/env python3
"""Build canonical isolation metadata and create detached OpenSSH signatures.

Release/runtime templates name artifacts by id; files in --artifacts-dir have
exactly those names. No private key is generated, copied, or stored by this tool.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

NAMESPACES = {
    "root": "duduclaw-root-metadata-v1@dudustudio",
    "release": "duduclaw-release-manifest-v1@dudustudio",
    "runtime": "duduclaw-runtime-manifest-v1@dudustudio",
    "emergency": "duduclaw-emergency-v1@dudustudio",
}
MAX_METADATA_BYTES = 256 * 1024


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate metadata key")
        result[key] = value
    return result


def reject_number(value):
    raise ValueError("metadata numbers must be integers")


def canonical_bytes(document):
    return json.dumps(document, sort_keys=True, ensure_ascii=False,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")


def bind_artifacts(document, directory):
    artifacts = document.get("artifacts")
    if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= 128:
        raise ValueError("expected 1 to 128 artifacts")
    seen = set()
    targets = set()
    for artifact in artifacts:
        artifact_id = artifact["id"]
        if (not isinstance(artifact_id, str)
                or re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", artifact_id) is None
                or artifact_id in (".", "..") or artifact_id in seen):
            raise ValueError("invalid or duplicate artifact id")
        seen.add(artifact_id)
        target = canonical_bytes(artifact["install_target"])
        if target in targets:
            raise ValueError("duplicate install target")
        targets.add(target)
        mode = artifact["mode"]
        if not isinstance(mode, int) or isinstance(mode, bool) or mode & ~0o755 or not mode & 0o400:
            raise ValueError("invalid artifact mode")
        fd = os.open(directory / artifact_id, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as source:
            before = os.fstat(source.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size == 0:
                raise ValueError("artifact must be a nonempty, unlinked regular file")
            digest = hashlib.sha256()
            length = 0
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
                length += len(chunk)
            after = os.fstat(source.fileno())
            if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                    after.st_size, after.st_mtime_ns, after.st_ctime_ns) or length != before.st_size:
                raise ValueError("artifact changed during hashing")
        artifact["sha256"] = list(digest.digest())
        artifact["size_bytes"] = length


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=NAMESPACES, default="release")
    parser.add_argument("--template", type=Path, required=True)
    parser.add_argument("--artifacts-dir", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--key", type=Path, action="append", required=True,
                        help="offline signing key; repeat for root threshold signatures")
    parser.add_argument("--ssh-keygen", default="ssh-keygen",
                        help="use Homebrew OpenSSH for FIDO2 support on macOS")
    args = parser.parse_args()
    raw = args.template.read_bytes()
    if len(raw) > MAX_METADATA_BYTES:
        parser.error("template exceeds metadata limit")
    document = json.loads(raw, object_pairs_hook=unique_object,
                          parse_float=reject_number, parse_constant=reject_number)
    if args.kind in ("release", "runtime"):
        if args.artifacts_dir is None:
            parser.error("--artifacts-dir is required for release/runtime metadata")
        bind_artifacts(document, args.artifacts_dir)
    if not 1 <= len(args.key) <= 6 or len(set(args.key)) != len(args.key):
        parser.error("use 1 to 6 distinct signing keys")
    payload = canonical_bytes(document)
    if len(payload) > MAX_METADATA_BYTES:
        parser.error("canonical metadata exceeds limit")
    # No output is published until every signer succeeds. Existing outputs are
    # never overwritten; release operators publish the resulting pair together.
    with tempfile.TemporaryDirectory(prefix="duduclaw-sign-") as temporary:
        stage = Path(temporary) / "metadata.json"
        stage.write_bytes(payload)
        signatures = []
        for key in args.key:
            subprocess.run([args.ssh_keygen, "-Y", "sign", "-f", str(key), "-n",
                            NAMESPACES[args.kind], "-O", "hashalg=sha512", str(stage)], check=True)
            detached = Path(str(stage) + ".sig")
            signatures.append(detached.read_text(encoding="ascii"))
            detached.unlink()
        signature_output = Path(str(args.output) + ".signatures.json")
        if args.output.exists() or signature_output.exists():
            parser.error("output already exists")
        with args.output.open("xb") as output:
            output.write(payload)
        with signature_output.open("x", encoding="ascii") as output:
            json.dump(signatures, output, separators=(",", ":"))
    print(hashlib.sha256(payload).hexdigest())


if __name__ == "__main__":
    main()
