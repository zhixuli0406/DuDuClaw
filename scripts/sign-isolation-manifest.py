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
key that is not a TEST-ONLY key file. Only that path takes --key <file>.

Key locations (production, i.e. without --test-signer): never on the command
line, because any local process can read another process's argv (ps). They come
from DUDUCLAW_ISOLATION_SIGNING_KEY_1 .. _<--key-count> or, for each one not
set, from an attended prompt on the controlling terminal (echoed: a location is
not a secret). --key is refused. ssh-keygen -Y sign needs `-f <path>`; it is
given only `<private 0700 temp dir>/signing-key`, a symlink to the real key
(plus signing-key.pub when the key has one), and the directory is removed right
after. Residual exposure: while ssh-keygen runs (the moment of signing, on the
offline ceremony machine) its argv shows that neutral temporary path, and the
symlink target is readable only by the operator's own user (and root).

Root threshold (--kind root and --kind bundle): after signing and before any
output is written, the key id of every signature is computed from the public key
embedded in the SSHSIG itself (SHA-256 of the key blob, like isolation-root-metadata
and sshsig.rs); the key ids must be pairwise different, each a root key of the
canonical root metadata (the document itself for --kind root; --root-metadata,
optionally pinned by --root-sha256, for --kind bundle), not revoked, and together
meet its root_threshold. Otherwise nothing is written. Distinct paths, links or
handles to one key are therefore one signer. --check-threshold re-runs exactly
this check on written files (scripts/release.sh does so before uploading).
"""
import argparse
import base64
import binascii
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
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
# Production key locations (never argv): DUDUCLAW_ISOLATION_SIGNING_KEY_<n>, n = 1..--key-count.
KEY_ENVIRONMENT = "DUDUCLAW_ISOLATION_SIGNING_KEY_{}"
# The only name ssh-keygen ever sees for a key (neutral_key_link).
NEUTRAL_KEY_NAME = "signing-key"
MAX_SIGNERS = 6
# duduclaw-updated sshsig.rs MAX_SIGNATURE_BYTES / duduclaw-isolation MAX_KEY_BLOB_BYTES.
MAX_SIGNATURE_BYTES = 16 * 1024
MAX_KEY_BLOB_BYTES = 4096
THRESHOLD_KINDS = ("root", "bundle")


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


def prompt_terminal(text):
    """One line from the controlling terminal, echo on (a key location is not a secret)."""
    try:
        with open("/dev/tty", "w", encoding="utf-8") as output, open("/dev/tty", encoding="utf-8") as source:
            output.write(text)
            output.flush()
            line = source.readline()
    except OSError as error:
        raise ValueError("no controlling terminal for the key prompt: set "
                         + KEY_ENVIRONMENT.format("<n>") + " instead") from error
    return line[:-1] if line.endswith("\n") else line


def key_locations(count, environ=None, prompt=prompt_terminal):
    """Production key locations, never from argv: DUDUCLAW_ISOLATION_SIGNING_KEY_<n>, else the prompt.

    Messages never contain a location.
    """
    environ = os.environ if environ is None else environ
    if not _integer(count, 1, MAX_SIGNERS):
        raise ValueError("use 1 to 6 signing keys")
    if KEY_ENVIRONMENT.format(count + 1) in environ:
        raise ValueError(f"{KEY_ENVIRONMENT.format(count + 1)} is set, but only {count} signing key(s) are used")
    locations = []
    for index in range(1, count + 1):
        value = environ.get(KEY_ENVIRONMENT.format(index))
        if value is None:
            value = prompt(f"Location of signing key {index} of {count} (key file or security-key handle; "
                           "a path, not a secret): ")
        if not value or any(character in value for character in "\0\r\n"):
            raise ValueError(f"signing key {index}: empty or malformed location")
        location = Path(os.path.abspath(value))
        if not location.is_file():
            raise ValueError(f"signing key {index} is not a file")
        locations.append(location)
    if len(set(locations)) != len(locations):
        raise ValueError("use distinct signing keys")
    return locations


@contextlib.contextmanager
def neutral_key_link(key):
    """A private 0700 directory holding only `signing-key` -> key (and `signing-key.pub` ->
    key.pub when it exists), removed on exit. ssh-keygen's argv then shows only this
    neutral temporary name, never the real location or file name of the key."""
    directory = Path(tempfile.mkdtemp(prefix="duduclaw-key-"))
    try:
        directory.chmod(0o700)
        link = directory / NEUTRAL_KEY_NAME
        os.symlink(os.path.abspath(key), link)
        public = Path(os.path.abspath(key) + ".pub")
        if public.is_file():
            os.symlink(public, directory / (NEUTRAL_KEY_NAME + ".pub"))
        yield link
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def signer_command(args, key, stage):
    """ssh-keygen (operator keys, FIDO2 via Homebrew OpenSSH) or the TEST-ONLY signer.

    Production: `key` must be the neutral link of neutral_key_link, never the real key.
    """
    if args.test_signer is not None:
        return [str(args.test_signer), "sign", str(key), args.kind, str(stage), str(stage) + ".sig"]
    if Path(key).name != NEUTRAL_KEY_NAME:
        raise ValueError("ssh-keygen is only ever given the neutral key link")
    return [args.ssh_keygen, "-Y", "sign", "-f", str(key), "-n", NAMESPACES[args.kind],
            "-O", "hashalg=sha512", str(stage)]


def _ssh_string(blob, offset):
    if offset + 4 > len(blob):
        raise ValueError("truncated SSH signature")
    end = offset + 4 + int.from_bytes(blob[offset:offset + 4], "big")
    if end > len(blob):
        raise ValueError("truncated SSH signature")
    return blob[offset + 4:end], end


def signature_public_blob(armor, namespace):
    """The public key blob inside an armored SSHSIG, parsed as strictly as sshsig.rs."""
    if not isinstance(armor, str) or len(armor) > MAX_SIGNATURE_BYTES:
        raise ValueError("malformed SSH signature")
    lines = (armor[:-1] if armor.endswith("\n") else armor).split("\n")
    body = lines[1:-1]
    if (len(lines) < 3 or lines[0] != "-----BEGIN SSH SIGNATURE-----" or lines[-1] != "-----END SSH SIGNATURE-----"
            or any(not line or len(line) > 76 or re.fullmatch(r"[A-Za-z0-9+/=]+", line) is None for line in body)):
        raise ValueError("malformed SSH signature armor")
    encoded = "".join(body)
    try:
        blob = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError("malformed SSH signature base64") from error
    if base64.b64encode(blob).decode("ascii") != encoded or blob[:6] != b"SSHSIG" \
            or blob[6:10] != (1).to_bytes(4, "big"):
        raise ValueError("malformed SSH signature")
    public, offset = _ssh_string(blob, 10)
    signed_namespace, offset = _ssh_string(blob, offset)
    reserved, offset = _ssh_string(blob, offset)
    hash_algorithm, offset = _ssh_string(blob, offset)
    _, offset = _ssh_string(blob, offset)
    if (offset != len(blob) or not 0 < len(public) <= MAX_KEY_BLOB_BYTES or reserved
            or hash_algorithm != b"sha512" or signed_namespace != namespace.encode("ascii")):
        raise ValueError("SSH signature has the wrong namespace, hash algorithm or shape")
    return public


def _byte_array(value, length=None):
    if (not isinstance(value, list) or (length is not None and len(value) != length)
            or not all(_integer(item, 0, 255) for item in value)):
        raise ValueError("root metadata byte arrays must be lists of 0..255")
    return bytes(value)


def root_signers(root_bytes):
    """({key_id: public blob}, revoked key ids, root_threshold) of canonical root metadata."""
    if len(root_bytes) > MAX_METADATA_BYTES:
        raise ValueError("root metadata exceeds the metadata limit")
    try:
        document = json.loads(root_bytes, object_pairs_hook=unique_object,
                              parse_float=reject_number, parse_constant=reject_number)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("root metadata is not JSON") from error
    if not isinstance(document, dict) or canonical_bytes(document) != root_bytes:
        raise ValueError("root metadata is not canonical")
    keys = document.get("root_keys")
    if not isinstance(keys, list) or not keys:
        raise ValueError("root metadata has no root keys")
    roots = {}
    for key in keys:
        if not isinstance(key, dict):
            raise ValueError("invalid root key")
        blob, key_id = _byte_array(key.get("public_blob")), _byte_array(key.get("key_id"), 32)
        if not 0 < len(blob) <= MAX_KEY_BLOB_BYTES or hashlib.sha256(blob).digest() != key_id or key_id in roots:
            raise ValueError("each root key id must be the SHA-256 of its own public key blob, once")
        roots[key_id] = blob
    revoked = document.get("revoked_key_ids")
    if not isinstance(revoked, list):
        raise ValueError("invalid revoked_key_ids")
    revoked = {_byte_array(item, 32) for item in revoked}
    threshold = document.get("root_threshold")
    if not _integer(threshold, 2, len(roots)):
        raise ValueError("root_threshold must be between 2 and the number of root keys")
    return roots, revoked, threshold


def check_root_threshold(signatures, root_bytes, namespace):
    """Same rules as duduclaw-updated trust::signatures/reject_unknown/threshold: every
    signer is identified by the key embedded in its signature, never by a path or handle.
    Returns the distinct signer key ids (hex)."""
    roots, revoked, threshold = root_signers(root_bytes)
    if not isinstance(signatures, list) or not 1 <= len(signatures) <= MAX_SIGNERS:
        raise ValueError("expected 1 to 6 signatures")
    seen = set()
    for armor in signatures:
        blob = signature_public_blob(armor, namespace)
        key_id = hashlib.sha256(blob).digest()
        if key_id in seen:
            raise ValueError("two signatures were made by the same key (another path, link or handle "
                             "to one key is not a second signer)")
        seen.add(key_id)
        if roots.get(key_id) != blob:
            raise ValueError("a signature was made by a key that is not a root key of the root metadata")
        if key_id in revoked:
            raise ValueError("a signature was made by a revoked root key")
    if len(seen) < threshold:
        raise ValueError(f"{len(seen)} distinct root key(s) do not meet the root threshold of {threshold}")
    return sorted(key_id.hex() for key_id in seen)


def read_bounded(path, limit):
    with open(path, "rb") as source:
        raw = source.read(limit + 1)
    if len(raw) > limit:
        raise ValueError("file exceeds the metadata limit")
    return raw


def read_root_metadata(path, expected_sha256=None):
    """Canonical root metadata bytes; with `expected_sha256`, they must hash to that pin."""
    raw = read_bounded(path, MAX_METADATA_BYTES)
    if expected_sha256 is not None:
        if re.fullmatch(r"[0-9a-f]{64}", expected_sha256) is None:
            raise ValueError("--root-sha256 must be 64 lowercase hex digits")
        if hashlib.sha256(raw).hexdigest() != expected_sha256:
            raise ValueError("root metadata does not match --root-sha256")
    root_signers(raw)
    return raw


def check_written_threshold(parser, args):
    """--check-threshold: verify a written document + signatures; never signs or writes."""
    if args.kind not in THRESHOLD_KINDS:
        parser.error("--check-threshold applies to --kind root or --kind bundle")
    if any(value is not None for value in (args.output, args.key, args.key_count, args.template, args.artifacts_dir,
                                           args.bundle, args.expires_at_unix, args.test_signer)):
        parser.error("--check-threshold takes only --kind, --signatures, --root-metadata and --root-sha256")
    try:
        document = read_bounded(args.check_threshold, MAX_METADATA_BYTES)
        signatures = json.loads(read_bounded(args.signatures or Path(str(args.check_threshold) + ".signatures.json"),
                                             MAX_METADATA_BYTES))
        if args.kind == "root":
            if args.root_metadata is not None:
                parser.error("--kind root is checked against itself; drop --root-metadata")
            root_bytes = read_root_metadata(args.check_threshold, args.root_sha256)
        else:
            if args.root_metadata is None:
                parser.error("--kind bundle requires --root-metadata")
            parsed = json.loads(document, object_pairs_hook=unique_object, parse_float=reject_number,
                                parse_constant=reject_number)
            if canonical_bytes(parsed) != document:
                raise ValueError("the authorization is not canonical")
            root_bytes = read_root_metadata(args.root_metadata, args.root_sha256)
        signers = check_root_threshold(signatures, root_bytes, NAMESPACES[args.kind])
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f"root threshold met by {len(signers)} distinct root keys")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--kind", choices=NAMESPACES, default="release")
    parser.add_argument("--template", type=Path, help="metadata template (every kind except bundle)")
    parser.add_argument("--artifacts-dir", type=Path)
    parser.add_argument("--bundle", type=Path, help="--kind bundle: the migration bundle tar to authorize")
    parser.add_argument("--expires-at-unix", type=int, help="--kind bundle: authorization expiry (Unix seconds)")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--key", type=Path, action="append",
                        help="TEST-ONLY, with --test-signer: throwaway test key file; repeat for threshold "
                             "signatures. Production refuses it (see key locations above)")
    parser.add_argument("--key-count", type=int,
                        help="production: number of signing keys (default 1; root/bundle: 2 or 3), each read "
                             "from " + KEY_ENVIRONMENT.format("<n>") + " or the terminal prompt")
    parser.add_argument("--root-metadata", type=Path,
                        help="--kind bundle: the pinned canonical root.json whose root keys must sign")
    parser.add_argument("--root-sha256", help="expected SHA-256 (bootstrap pin) of the root metadata")
    parser.add_argument("--check-threshold", type=Path,
                        help="verify an already written root/bundle document and its signatures; signs nothing")
    parser.add_argument("--signatures", type=Path,
                        help="with --check-threshold: signatures JSON (default <document>.signatures.json)")
    parser.add_argument("--ssh-keygen", default="ssh-keygen",
                        help="use Homebrew OpenSSH for FIDO2 support on macOS")
    parser.add_argument("--test-signer", type=Path,
                        help="TEST-ONLY: duduclaw-isolation-test-signer instead of ssh-keygen (test key set only)")
    args = parser.parse_args()
    if args.test_signer is not None and args.ssh_keygen != "ssh-keygen":
        parser.error("--test-signer and --ssh-keygen are mutually exclusive")
    if args.check_threshold is not None:
        check_written_threshold(parser, args)
        return
    if args.signatures is not None:
        parser.error("--signatures belongs to --check-threshold")
    if args.output is None:
        parser.error("--output is required")
    if args.test_signer is None:
        if args.key:
            parser.error("production signing never takes --key: key locations come from "
                         + KEY_ENVIRONMENT.format("<n>") + " or the terminal prompt, because argv is visible "
                         "to other local processes (--key is only for --test-signer)")
        count = 1 if args.key_count is None else args.key_count
    else:
        if args.key_count is not None:
            parser.error("--key-count is for production signing; --test-signer takes --key")
        if not args.key or len(set(args.key)) != len(args.key):
            parser.error("--test-signer needs distinct --key files")
        count = len(args.key)
    if not _integer(count, 1, MAX_SIGNERS):
        parser.error("use 1 to 6 distinct signing keys")
    if args.kind != "bundle" and (args.root_metadata is not None or args.root_sha256 is not None):
        parser.error("--root-metadata/--root-sha256 belong to --kind bundle (a root is checked against itself)")
    if args.kind == "bundle":
        if args.template is not None or args.artifacts_dir is not None:
            parser.error("--kind bundle takes --bundle, never a template or artifacts directory")
        if args.bundle is None or args.expires_at_unix is None:
            parser.error("--kind bundle requires --bundle and --expires-at-unix")
        if not 2 <= count <= 3:
            parser.error("--kind bundle needs a root threshold: 2 or 3 distinct root keys")
        try:
            document = bundle_authorization(args.bundle, args.expires_at_unix)
        except (OSError, ValueError) as error:
            parser.error(str(error))
        if args.root_metadata is None:
            parser.error("--kind bundle requires --root-metadata (the pinned root.json whose root keys must sign)")
        try:
            root_bytes = read_root_metadata(args.root_metadata, args.root_sha256)
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
    if args.kind == "root":
        # The root threshold of a root is checked against the root being signed.
        root_bytes = payload
    if args.kind in THRESHOLD_KINDS:
        try:
            _, _, threshold = root_signers(root_bytes)
        except ValueError as error:
            parser.error(str(error))
        if count < threshold:
            parser.error(f"{count} signing key(s) cannot meet the root threshold of {threshold}")
    signature_output = Path(str(args.output) + ".signatures.json")
    if args.output.exists() or signature_output.exists():
        parser.error("output already exists")
    # Every refusal above happens before a key location is asked for or a key is touched.
    try:
        keys = args.key if args.test_signer is not None else key_locations(count)
    except ValueError as error:
        parser.error(str(error))
    # No output is published until every signer succeeds. Existing outputs are
    # never overwritten; release operators publish the resulting pair together.
    with tempfile.TemporaryDirectory(prefix="duduclaw-sign-") as temporary:
        stage = Path(temporary) / "metadata.json"
        stage.write_bytes(payload)
        signatures = []
        for key in keys:
            if args.test_signer is not None:
                subprocess.run(signer_command(args, key, stage), check=True)
            else:
                with neutral_key_link(key) as link:
                    subprocess.run(signer_command(args, link, stage), check=True)
            detached = Path(str(stage) + ".sig")
            signatures.append(detached.read_text(encoding="ascii"))
            detached.unlink()
        if args.kind in THRESHOLD_KINDS:
            # R2-5: signers are the keys embedded in the signatures, checked against the
            # root metadata before anything is written (so nothing is left to delete).
            try:
                check_root_threshold(signatures, root_bytes, NAMESPACES[args.kind])
            except ValueError as error:
                parser.error(f"{error}; nothing was written")
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
