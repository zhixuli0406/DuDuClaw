#!/usr/bin/env python3
"""Build canonical isolation metadata and create detached OpenSSH signatures.

Release/runtime templates name artifacts by id; files in --artifacts-dir have
exactly those names. No private key is generated, copied, or stored by this tool:
`ssh-keygen -Y sign` reads the operator's key file (or FIDO2 key handle) itself
and asks for its passphrase / PIN on the terminal.

--kind bundle (plan §3.2, R2 §4.2) authorizes one migration bundle tar: the input
is the tar itself (--bundle, fixed name duduclaw-isolated-linux-<arch>-<x.y.z>.tar),
the output is the canonical BundleAuthorization {schema_version, product, version,
arch, bundle_sha256, bundle_size, expires_at_unix} plus its root-threshold
signatures (2 or 3 distinct root keys) under duduclaw-migration-bundle-v1@dudustudio.
Owner decision Q6: only v1.83.0 ships a migration bundle (scripts/release.sh
isolation-sign gates this).

--test-signer <duduclaw-isolation-test-signer> replaces ssh-keygen for the
TEST-ONLY key set of scripts/isolation-test-keys.sh; that binary refuses every
key that is not a TEST-ONLY key file.
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
import time

NAMESPACES = {
    "root": "duduclaw-root-metadata-v1@dudustudio",
    "release": "duduclaw-release-manifest-v1@dudustudio",
    "runtime": "duduclaw-runtime-manifest-v1@dudustudio",
    "emergency": "duduclaw-emergency-v1@dudustudio",
    # Root threshold over a BundleAuthorization (duduclaw-isolation
    # MIGRATION_BUNDLE_SSHSIG_NAMESPACE); never accepted for any other document.
    "bundle": "duduclaw-migration-bundle-v1@dudustudio",
}
MAX_METADATA_BYTES = 256 * 1024
# duduclaw-isolation MAX_MIGRATION_BUNDLE_BYTES.
MAX_BUNDLE_BYTES = 2 * 1024 * 1024 * 1024
BUNDLE_NAME = re.compile(r"duduclaw-isolated-linux-(x86_64|aarch64)-(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\."
                         r"(0|[1-9][0-9]{0,8})\.tar")


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


POLICY_DOMAIN_V2 = "duduclaw-isolation-policy-v2"
HELPER_BINDING_KEYS = {"profile", "slot", "seccomp", "seccomp_digest", "self_only", "memory_bytes", "tasks",
                       "cpu_quota_percent", "open_files", "file_size_bytes", "timeout_ms", "filesystem", "egress"}
HELPER_FS = {"work", "work_recordings", "system_ro"}
PRCTL_SELF_ONLY = {1, 2, 3, 4, 7, 16, 21, 27, 30, 38, 39, 42}


def _integer(value, low, high):
    return isinstance(value, int) and not isinstance(value, bool) and low <= value <= high


def validate_policy_document(document):
    """Offline shape check of the signed isolation policy artifact.

    spawnd re-runs every publication check (HX D8 revision section 2.3) on the
    bytes; this only stops a malformed template before it is bound by the
    release manifest. v2 carries its domain and version in the signed bytes;
    v1 has neither a domain nor a launcher.
    """
    if not isinstance(document, dict):
        raise ValueError("policy must be an object")
    version = document.get("version")
    if version == 1:
        if set(document) != {"version", "runtimes", "native"}:
            raise ValueError("v1 policy has exactly version/runtimes/native")
    elif version == 2:
        if set(document) != {"domain", "version", "runtimes", "native"} or document["domain"] != POLICY_DOMAIN_V2:
            raise ValueError("v2 policy requires its signed domain")
    else:
        raise ValueError("unsupported policy version")
    native = document.get("native")
    if not isinstance(native, list) or not isinstance(document.get("runtimes"), list) \
            or len(native) != len(document["runtimes"]):
        raise ValueError("one native record per runtime")
    for record in native:
        launcher = record.get("launcher") if isinstance(record, dict) else None
        if launcher is None:
            continue
        if version != 2:
            raise ValueError("launcher requires policy v2")
        if not isinstance(launcher, dict) or set(launcher) != {"reserve", "helpers", "helper_tmpfs_bytes"}:
            raise ValueError("invalid launcher record")
        if not _integer(launcher["helper_tmpfs_bytes"], 1, record.get("tmpfs_bytes", 0)):
            raise ValueError("helper tmpfs exceeds the parent tmpfs budget")
        helpers = launcher["helpers"]
        if not isinstance(helpers, list) or not 1 <= len(helpers) <= 16:
            raise ValueError("1 to 16 helper bindings")
        profiles = set()
        for binding in helpers:
            if not isinstance(binding, dict) or not set(binding) <= HELPER_BINDING_KEYS \
                    or not (HELPER_BINDING_KEYS - {"self_only"}) <= set(binding):
                raise ValueError("invalid helper binding fields")
            profile = canonical_bytes(binding["profile"])
            if profile in profiles or binding["profile"] == record.get("profile"):
                raise ValueError("duplicate or self helper binding")
            profiles.add(profile)
            recording = binding["slot"] == "recording"
            if binding["slot"] not in ("foreground", "recording") or binding["egress"] is not recording:
                raise ValueError("recording slot iff egress helper")
            grants = binding["filesystem"]
            if not isinstance(grants, list) or not grants or len(set(grants)) != len(grants) \
                    or not set(grants) <= HELPER_FS or ("work" in grants) == recording \
                    or ("work_recordings" in grants) != recording:
                raise ValueError("invalid helper filesystem grants")
            if not _integer(binding["timeout_ms"], 1, 300_000):
                raise ValueError("helper timeout must be 1..300000 ms")
            for key in ("memory_bytes", "tasks", "cpu_quota_percent", "open_files", "file_size_bytes"):
                if not _integer(binding[key], 1, 2 ** 64 - 1):
                    raise ValueError("helper limits must be positive integers")
            for call in binding.get("self_only", []):
                if call == {"kind": "prlimit64_self_only"}:
                    continue
                options = call.get("options") if isinstance(call, dict) and set(call) == {"kind", "options"} \
                    and call.get("kind") == "prctl" else None
                if not isinstance(options, list) or not options or len(set(options)) != len(options) \
                        or not all(_integer(o, 0, 2 ** 32 - 1) and o in PRCTL_SELF_ONLY for o in options):
                    raise ValueError("invalid self-only seccomp exception")
    return document


def hash_regular_file(path, limit):
    """(sha256 bytes, size) of a nonempty, single-link regular file; refuses symlinks and changes."""
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0))
    with os.fdopen(fd, "rb") as source:
        before = os.fstat(source.fileno())
        if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
                or not 0 < before.st_size <= limit):
            raise ValueError("bundle must be a nonempty, unlinked regular file within the size limit")
        digest = hashlib.sha256()
        length = 0
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
            length += len(chunk)
        after = os.fstat(source.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
                after.st_size, after.st_mtime_ns, after.st_ctime_ns) or length != before.st_size:
            raise ValueError("bundle changed during hashing")
    return digest.digest(), length


def bundle_authorization(path, expires_at_unix, now=None):
    """Canonical BundleAuthorization for the tar at `path` (duduclaw-isolation update.rs).

    Version and architecture come only from the fixed file name the verifier
    opens (plan §3.2); digest and size from the bytes themselves.
    """
    match = BUNDLE_NAME.fullmatch(Path(path).name)
    if match is None:
        raise ValueError("bundle file name must be duduclaw-isolated-linux-<x86_64|aarch64>-<x.y.z>.tar")
    arch, major, minor, patch = match[1], int(match[2]), int(match[3]), int(match[4])
    if max(major, minor, patch) > 2 ** 32 - 1:
        raise ValueError("bundle version component out of range")
    now = int(time.time()) if now is None else now
    if (not isinstance(expires_at_unix, int) or isinstance(expires_at_unix, bool)
            or not now < expires_at_unix < 2 ** 63):
        raise ValueError("bundle authorization must expire in the future")
    digest, size = hash_regular_file(path, MAX_BUNDLE_BYTES)
    return {"schema_version": 1, "product": "duduclaw",
            "version": {"major": major, "minor": minor, "patch": patch}, "arch": arch,
            "bundle_sha256": list(digest), "bundle_size": size, "expires_at_unix": expires_at_unix}


def signer_command(args, key, stage):
    """ssh-keygen (operator keys, FIDO2 via Homebrew OpenSSH) or the TEST-ONLY signer."""
    if args.test_signer is not None:
        return [str(args.test_signer), "sign", str(key), args.kind, str(stage), str(stage) + ".sig"]
    return [args.ssh_keygen, "-Y", "sign", "-f", str(key), "-n", NAMESPACES[args.kind],
            "-O", "hashalg=sha512", str(stage)]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--kind", choices=NAMESPACES, default="release")
    parser.add_argument("--template", type=Path, help="metadata template (every kind except bundle)")
    parser.add_argument("--artifacts-dir", type=Path)
    parser.add_argument("--bundle", type=Path, help="--kind bundle: the migration bundle tar to authorize")
    parser.add_argument("--expires-at-unix", type=int, help="--kind bundle: authorization expiry (Unix seconds)")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--key", type=Path, action="append", required=True,
                        help="offline signing key; repeat for root threshold signatures")
    parser.add_argument("--ssh-keygen", default="ssh-keygen",
                        help="use Homebrew OpenSSH for FIDO2 support on macOS")
    parser.add_argument("--test-signer", type=Path,
                        help="TEST-ONLY: duduclaw-isolation-test-signer instead of ssh-keygen (test key set only)")
    args = parser.parse_args()
    if args.test_signer is not None and args.ssh_keygen != "ssh-keygen":
        parser.error("--test-signer and --ssh-keygen are mutually exclusive")
    if not 1 <= len(args.key) <= 6 or len(set(args.key)) != len(args.key):
        parser.error("use 1 to 6 distinct signing keys")
    if args.kind == "bundle":
        if args.template is not None or args.artifacts_dir is not None:
            parser.error("--kind bundle takes --bundle, never a template or artifacts directory")
        if args.bundle is None or args.expires_at_unix is None:
            parser.error("--kind bundle requires --bundle and --expires-at-unix")
        if not 2 <= len(args.key) <= 3:
            parser.error("--kind bundle needs a root threshold: 2 or 3 distinct root keys")
        try:
            document = bundle_authorization(args.bundle, args.expires_at_unix)
        except (OSError, ValueError) as error:
            parser.error(str(error))
    else:
        if args.bundle is not None or args.expires_at_unix is not None:
            parser.error("--bundle/--expires-at-unix belong to --kind bundle")
        if args.template is None:
            parser.error("--template is required")
        raw = args.template.read_bytes()
        if len(raw) > MAX_METADATA_BYTES:
            parser.error("template exceeds metadata limit")
        document = json.loads(raw, object_pairs_hook=unique_object,
                              parse_float=reject_number, parse_constant=reject_number)
        if args.kind in ("release", "runtime"):
            if args.artifacts_dir is None:
                parser.error("--artifacts-dir is required for release/runtime metadata")
            bind_artifacts(document, args.artifacts_dir)
    payload = canonical_bytes(document)
    if len(payload) > MAX_METADATA_BYTES:
        parser.error("canonical metadata exceeds limit")
    signature_output = Path(str(args.output) + ".signatures.json")
    if args.output.exists() or signature_output.exists():
        parser.error("output already exists")
    # No output is published until every signer succeeds. Existing outputs are
    # never overwritten; release operators publish the resulting pair together.
    with tempfile.TemporaryDirectory(prefix="duduclaw-sign-") as temporary:
        stage = Path(temporary) / "metadata.json"
        stage.write_bytes(payload)
        signatures = []
        for key in args.key:
            subprocess.run(signer_command(args, key, stage), check=True)
            detached = Path(str(stage) + ".sig")
            signatures.append(detached.read_text(encoding="ascii"))
            detached.unlink()
        if args.kind == "bundle":
            # The authorization binds these exact bytes: the tar must not have
            # changed while the operator touched the keys.
            if hash_regular_file(args.bundle, MAX_BUNDLE_BYTES) != (bytes(document["bundle_sha256"]),
                                                                     document["bundle_size"]):
                parser.error("bundle changed while it was being authorized; nothing was written")
        with args.output.open("xb") as output:
            output.write(payload)
        with signature_output.open("x", encoding="ascii") as output:
            json.dump(signatures, output, separators=(",", ":"))
    print(hashlib.sha256(payload).hexdigest())


if __name__ == "__main__":
    main()
