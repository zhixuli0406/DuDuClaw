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

Key locations (production, i.e. without --test-signer): ONLY on stdin, one
location per line, signing key 1 first (for --kind bundle: R1 then R2), exactly
--key-count lines. Never on the command line (any local process can read argv)
and never from the environment: a DUDUCLAW_ISOLATION_*KEY* variable in the
environment is refused. On a terminal each line is prompted for (echoed: a
location is not a secret); the ceremony (started as scripts/isolation-sign, which
runs scripts/release.sh isolation-sign) pipes the locations it read at its own
prompt. --key is refused. ssh-keygen -Y sign needs `-f
<path>`; it is given only `<private 0700 temp dir>/signing-key`, a symlink to the
real key (plus signing-key.pub when the key has one), and the directory is
removed right after. Every child (ssh-keygen, minisign, the TEST signer) runs with
an explicit environment: PATH=/usr/bin:/bin:/usr/sbin:/sbin plus HOME, LANG,
LC_ALL, LC_CTYPE, TERM and TMPDIR when set, nothing else (no key variable, no
SSH_AUTH_SOCK, SSH_ASKPASS or DISPLAY), with the controlling terminal as stdin
for its PIN/passphrase prompt. The one addition: `ssh-keygen -Y sign` alone gets
SSH_SK_HELPER, the audited ssh-sk-helper of the SELECTED ssh-keygen's own
installation (--ssh-sk-helper, else libexec/ssh-sk-helper next to the resolved
ssh-keygen; FIDO2 keys need it), or, when there is none, a path inside the
private key-link directory that does not exist, so ssh-keygen can never start
another helper. A key whose signing-key.pub names a security key (sk-...), or
that has no .pub at all, is refused without a helper. Tools named without a path
are looked up only in /usr/bin, /bin, /usr/sbin, /sbin, /opt/homebrew/bin and
/usr/local/bin, never in PATH. Residual exposure: while a signer runs its argv
shows the neutral temporary path, and the symlink target is readable by the
operator's own user (and root).

--minisign-sign <file> --trusted-comment <text> --output <file.minisig>: the
legacy release key (minisign) signs <file>, its location read from stdin the same
way and handed to minisign only as the neutral link.

Root threshold (--kind root and --kind bundle): after signing and before any
output is written, the key id of every signature is computed from the public key
embedded in the SSHSIG itself (SHA-256 of the key blob, like isolation-root-metadata
and sshsig.rs); the key ids must be pairwise different, each a root key of the
canonical root metadata (the document itself for --kind root; --root-metadata,
optionally pinned by --root-sha256, for --kind bundle), not revoked, and together
meet its root_threshold. Each signature must also have the shape the Rust verifier
(crates/duduclaw-updated/src/sshsig.rs) accepts (ssh-ed25519 or
sk-ssh-ed25519@openssh.com matching the registered key, role application, 64-byte
signature with canonical S, no small-order point, security-key flags exactly
UP|UV, no trailing bytes) and must VERIFY cryptographically over the exact
canonical bytes and namespace: `ssh-keygen -Y verify` against an allowed-signers
file listing only the root key with that key id (principal = key id); with
--test-signer, `<test signer> verify`, which runs the crate's production verifier.
Otherwise nothing is written. Distinct paths, links or handles to one key are
therefore one signer. --check-threshold re-runs exactly this check on written
files (scripts/release.sh does so before uploading); for --kind bundle, --bundle
<tar> also requires the tar to be exactly the authorized one (name, digest, size).

Target key (--kind release / runtime with --root-metadata, optionally pinned by
--root-sha256): after signing and before any output is written, the single
signature must be made by the root metadata's target_key (key id from the
embedded public key, not revoked), have the shape the Rust verifier accepts and
VERIFY with `ssh-keygen -Y verify` against an allowed-signers file listing only
that key (TEST: `<test signer> verify`). A wrong but valid key therefore writes
nothing. --check-target <manifest> re-runs this check on written files; with
--artifacts-dir <dir> every artifact the manifest binds must be <dir>/<prefix><id>
(prefix: the manifest file name without "manifest.json") with exactly the signed
digest and size (the signing-time binding, recomputed).
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
import sys
import tempfile
import time
import unicodedata

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
# Production key locations come only from stdin (read_key_locations); a variable of this
# shape in the environment is refused, so no key location can be inherited by any child.
KEY_VARIABLE = re.compile(r"DUDUCLAW_ISOLATION_[A-Za-z0-9_]*KEY[A-Za-z0-9_]*")
MAX_LOCATION_BYTES = 4096
# The only name ssh-keygen / minisign ever see for a key (neutral_key_link).
NEUTRAL_KEY_NAME = "signing-key"
MAX_SIGNERS = 6
# duduclaw-updated sshsig.rs MAX_SIGNATURE_BYTES / duduclaw-isolation MAX_KEY_BLOB_BYTES.
MAX_SIGNATURE_BYTES = 16 * 1024
MAX_KEY_BLOB_BYTES = 4096
THRESHOLD_KINDS = ("root", "bundle")
# Signed by the target key of the root metadata (check_target_signature).
TARGET_KINDS = ("release", "runtime")
# Every child gets exactly this environment (child_environment): a fixed PATH plus these
# variables when set. Never a key location, SSH_AUTH_SOCK, SSH_ASKPASS, DISPLAY; SSH_SK_HELPER
# only for ssh-keygen -Y sign (signing_environment), and only the audited helper.
SYSTEM_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
CHILD_VARIABLES = ("HOME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR")
SK_HELPER_NAME = "ssh-sk-helper"
# ssh-keygen -Y sign's SSH_SK_HELPER when its installation has no helper: inside the private
# 0700 key-link directory, never created, so ssh-keygen cannot start any other helper.
NO_SK_HELPER_NAME = "no-ssh-sk-helper"
# A tool named without a path is looked up only here (never PATH): system directories first.
TOOL_DIRECTORIES = ("/usr/bin", "/bin", "/usr/sbin", "/sbin", "/opt/homebrew/bin", "/usr/local/bin")
# The accept set of duduclaw-updated sshsig.rs (SshSignature::verify, validate_key) and
# duduclaw-isolation update.rs (ROOT/TARGET_SK_APPLICATION, REQUIRED_SK_FLAGS).
ED25519 = b"ssh-ed25519"
SK_ED25519 = b"sk-ssh-ed25519@openssh.com"
ROOT_SK_APPLICATION = "ssh:duduclaw-root"
TARGET_SK_APPLICATION = "ssh:duduclaw-target"
REQUIRED_SK_FLAGS = 0x01 | 0x04
ED25519_GROUP_ORDER = 2 ** 252 + 27742317777372353535851937790883648493
_FIELD = 2 ** 255 - 19
_CURVE_D = (-121665 * pow(121666, _FIELD - 2, _FIELD)) % _FIELD
_SQRT_MINUS_ONE = pow(2, (_FIELD - 1) // 4, _FIELD)


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


def bind_artifacts(document, directory, prefix=""):
    """Binds each artifact's SHA-256 and size from <directory>/<prefix><id> (signing: no prefix;
    check_bound_artifacts: the packager's published names)."""
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
        fd = os.open(directory / (prefix + artifact_id), os.O_RDONLY | os.O_NOFOLLOW)
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
POLICY_DOMAIN_V3 = "duduclaw-isolation-policy-v3"
# Policy v3 step range (spawnd `policy::TurnLimit`): 1 <= min <= default <= max <= 1000.
TURN_LIMIT_KEYS = {"min", "max", "default"}
TURN_LIMIT_CEILING = 1000
# Working default for a v3 template: PENDING OWNER DECISION Q12 (min 1, default 50, max 200).
TURN_LIMIT_WORKING_DEFAULT = {"min": 1, "default": 50, "max": 200}
CLAUDE_MCP_SUFFIX = ["--strict-mcp-config", "--mcp-config", "/config/mcp.json"]
# The canonical signed Claude MCP shape (spawnd `policy::validate_claude_arguments`,
# `gateway_plan::claude_arguments`): this prefix, then `--max-turns 50` only
# without a range, then optionally `--model <m>`, then the fixed MCP suffix.
CLAUDE_FIXED_PREFIX = ["-p", "--input-format", "text", "--output-format", "stream-json", "--verbose",
                       "--system-prompt-file", "/proc/self/fd/5", "--tools", "", "--allowedTools", "mcp__duduclaw__*"]
# Options of that shape which consume the next element (spawnd `CLAUDE_VALUE_OPTIONS`).
CLAUDE_VALUE_OPTIONS = {"--input-format", "--output-format", "--system-prompt-file", "--tools",
                        "--allowedTools", "--max-turns", "--model", "--mcp-config"}
LEGACY_FIXED_MAX_TURNS = "50"
HELPER_BINDING_KEYS = {"profile", "slot", "seccomp", "seccomp_digest", "self_only", "memory_bytes", "tasks",
                       "cpu_quota_percent", "open_files", "file_size_bytes", "timeout_ms", "filesystem", "egress"}
HELPER_FS = {"work", "work_recordings", "system_ro"}
PRCTL_SELF_ONLY = {1, 2, 3, 4, 7, 16, 21, 27, 30, 38, 39, 42}


def _integer(value, low, high):
    return isinstance(value, int) and not isinstance(value, bool) and low <= value <= high


def _plain_model(model):
    return (isinstance(model, str) and model != "" and len(model.encode("utf-8")) <= 256
            and not model.startswith("-") and model.strip() == model
            and not any(unicodedata.category(char) == "Cc" for char in model))


def validate_claude_arguments(arguments, ranged):
    """Signed arguments of a Claude MCP record, in every policy version.

    Without a range: exactly prefix + `--max-turns 50` + [`--model m`] + suffix.
    With a range (v3): prefix + [`--model m`] + suffix and no `--max-turns` in
    any spelling; spawnd inserts `--max-turns <n>` right before the suffix, so
    no bare `--` may precede it and it must not follow a value-taking option.
    """
    suffix = len(CLAUDE_MCP_SUFFIX)
    if not isinstance(arguments, list) or not all(isinstance(arg, str) for arg in arguments) \
            or len(arguments) < suffix or arguments[-suffix:] != CLAUDE_MCP_SUFFIX:
        raise ValueError("Claude MCP arguments must end with the fixed MCP suffix")
    head = arguments[:-suffix]
    if "--" in head or not head or head[-1] in CLAUDE_VALUE_OPTIONS:
        raise ValueError("the --max-turns insertion point must stay in the CLI option area")
    if sum(arg.startswith("--max-turns") for arg in arguments) != (0 if ranged else 1):
        raise ValueError("a Claude MCP record needs exactly one of turn_limit or the fixed --max-turns 50")
    rest = head[len(CLAUDE_FIXED_PREFIX):]
    if head[:len(CLAUDE_FIXED_PREFIX)] != CLAUDE_FIXED_PREFIX:
        raise ValueError("Claude MCP arguments are not the canonical shape")
    if not ranged:
        if rest[:2] != ["--max-turns", LEGACY_FIXED_MAX_TURNS]:
            raise ValueError("the fixed step limit is exactly --max-turns 50 after the input protocol")
        rest = rest[2:]
    if rest and not (len(rest) == 2 and rest[0] == "--model" and _plain_model(rest[1])):
        raise ValueError("Claude MCP arguments are not the canonical shape")


def validate_turn_limit(runtime, version):
    """Step limit of one runtime record (spawnd re-checks all of it).

    Every Claude MCP record, in every version, has exactly one enforceable
    step limit: absent `turn_limit` with the canonical fixed `--max-turns 50`,
    or (v3 only) a valid `turn_limit` object with no `--max-turns` at all.
    The key is refused before v3 and as an explicit null; any invalid value
    makes the whole policy invalid.
    """
    if not isinstance(runtime, dict):
        return
    claude_mcp = runtime.get("runtime") == {"kind": "claude"} and runtime.get("mcp") is True
    ranged = "turn_limit" in runtime
    if ranged:
        limit = runtime["turn_limit"]
        if version < 3:
            raise ValueError("turn_limit requires policy v3")
        if not isinstance(limit, dict) or set(limit) != TURN_LIMIT_KEYS \
                or not all(_integer(limit[key], 1, TURN_LIMIT_CEILING) for key in TURN_LIMIT_KEYS) \
                or not limit["min"] <= limit["default"] <= limit["max"]:
            raise ValueError("turn_limit must satisfy 1 <= min <= default <= max <= 1000")
        if not claude_mcp:
            raise ValueError("turn_limit requires a Claude MCP record")
    if claude_mcp:
        validate_claude_arguments(runtime.get("fixed_args"), ranged)


def validate_policy_document(document):
    """Offline shape check of the signed isolation policy artifact.

    spawnd re-runs every publication check (HX D8 revision section 2.3) on the
    bytes; this only stops a malformed template before it is bound by the
    release manifest. v2 and v3 carry their domain and version in the signed
    bytes; v1 has neither a domain nor a launcher; only v3 has step ranges.
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
    elif version == 3:
        if set(document) != {"domain", "version", "runtimes", "native"} or document["domain"] != POLICY_DOMAIN_V3:
            raise ValueError("v3 policy requires its signed domain")
    else:
        raise ValueError("unsupported policy version")
    native = document.get("native")
    if not isinstance(native, list) or not isinstance(document.get("runtimes"), list) \
            or len(native) != len(document["runtimes"]):
        raise ValueError("one native record per runtime")
    for runtime in document["runtimes"]:
        validate_turn_limit(runtime, version)
    for record in native:
        launcher = record.get("launcher") if isinstance(record, dict) else None
        if launcher is None:
            continue
        if version < 2:
            raise ValueError("launcher requires policy v2 or later")
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


def refuse_key_variables(environ=None):
    """Key locations never come from (or travel through) the environment."""
    environ = os.environ if environ is None else environ
    names = sorted(name for name in environ if KEY_VARIABLE.fullmatch(name))
    if names:
        raise ValueError("key locations are never read from the environment (they are handed over on stdin); "
                         "unset " + ", ".join(names))


def child_environment(environ=None):
    """The complete environment of every child process: a fixed PATH and CHILD_VARIABLES, nothing else."""
    environ = os.environ if environ is None else environ
    result = {"PATH": SYSTEM_PATH}
    for name in CHILD_VARIABLES:
        value = environ.get(name)
        if value:
            result[name] = value
    return result


def resolve_tool(value, what):
    """Absolute path of a tool: an absolute path as given, or a bare name looked up only in
    TOOL_DIRECTORIES (never PATH). scripts/release.sh always passes absolute paths."""
    value = os.fspath(value) if value is not None else ""
    if not value or "\0" in value or "\n" in value:
        raise ValueError(f"{what}: invalid tool path")
    if os.path.isabs(value):
        candidate = value
    elif "/" in value:
        raise ValueError(f"{what} must be an absolute path or a bare name")
    else:
        candidate = shutil.which(value, path=os.pathsep.join(TOOL_DIRECTORIES))
        if candidate is None:
            raise ValueError(f"{what}: {value} is not in {', '.join(TOOL_DIRECTORIES)}")
    if not os.path.isfile(candidate) or not os.access(candidate, os.X_OK):
        raise ValueError(f"{what} is not an executable file")
    return candidate


def audited_tool(candidate, what):
    """scripts/release.sh's tool audit (ISOLATION_TOOL_AUDIT_PY): an absolute path to a regular
    executable (after resolving links) owned by root or you and not group/other-writable, in a
    directory (as named and resolved) owned by root or you and not group/other-writable.
    Returns the resolved path."""
    uid = os.geteuid()

    def directory_ok(path):
        try:
            info = os.stat(path)
        except OSError:
            return False
        return stat.S_ISDIR(info.st_mode) and info.st_uid in (0, uid) and not info.st_mode & 0o022
    candidate = os.fspath(candidate)
    real = os.path.realpath(candidate)
    try:
        info = os.stat(real)
        ok = (os.path.isabs(candidate) and stat.S_ISREG(info.st_mode) and os.access(real, os.X_OK)
              and info.st_uid in (0, uid) and not info.st_mode & 0o022 and "\n" not in real
              and directory_ok(os.path.dirname(candidate)) and directory_ok(os.path.dirname(real)))
    except OSError:
        ok = False
    if not ok:
        raise ValueError(f"{what}: refusing {candidate}: not a regular executable owned by root or you, or its "
                         "directory is group/other-writable or owned by someone else")
    return real


def sk_helper_candidates(ssh_keygen):
    """Where the ssh-sk-helper of THIS ssh-keygen's installation lives: libexec/ next to its
    resolved path (/usr/bin/ssh-keygen -> /usr/libexec; Homebrew: the Cellar keg's libexec, the
    compiled-in default of that ssh-keygen), then <prefix>/libexec of the path as given."""
    paths = (os.path.realpath(ssh_keygen), os.path.abspath(ssh_keygen))
    candidates = []
    for path in paths:
        candidate = os.path.join(os.path.dirname(os.path.dirname(path)), "libexec", SK_HELPER_NAME)
        if candidate not in candidates:
            candidates.append(candidate)
    return candidates


def resolve_sk_helper(ssh_keygen, given=None):
    """The audited absolute ssh-sk-helper for `ssh_keygen`, or None when its installation has none.
    `given` (scripts/release.sh resolves it next to the selected ssh-keygen and audits it the same
    way) must be an absolute libexec/ssh-sk-helper and pass the same audit; a present but
    unaudited candidate refuses rather than being skipped."""
    if given is not None:
        given = os.fspath(given)
        if (not os.path.isabs(given) or os.path.basename(given) != SK_HELPER_NAME
                or os.path.basename(os.path.dirname(os.path.realpath(given))) != "libexec"):
            raise ValueError("--ssh-sk-helper must be the absolute path of libexec/ssh-sk-helper of the "
                             "selected ssh-keygen's installation")
        return audited_tool(given, "--ssh-sk-helper")
    for candidate in sk_helper_candidates(ssh_keygen):
        if os.path.lexists(candidate):
            return audited_tool(candidate, "ssh-sk-helper")
    return None


def may_be_security_key(key):
    """Whether ssh-keygen may start ssh-sk-helper for `key`: its PUBLIC half <key>.pub names a
    security-key algorithm (sk-...), or there is no readable <key>.pub (unknown: fail closed).
    The key file itself is only ever opened by ssh-keygen."""
    try:
        with open(os.path.abspath(key) + ".pub", "rb") as source:
            fields = source.read(256).split(None, 1)
    except OSError:
        return True
    return not fields or fields[0].startswith(b"sk-")


def signing_environment(link, helper):
    """child_environment() plus SSH_SK_HELPER, for `ssh-keygen -Y sign` ONLY: the audited helper of
    the selected installation, or a never-created path inside the private key-link directory, so
    ssh-keygen can never start a helper outside the audited tool set."""
    environment = child_environment()
    environment["SSH_SK_HELPER"] = str(helper) if helper is not None else str(Path(link).parent / NO_SK_HELPER_NAME)
    return environment


def read_key_locations(count, stream=None, interactive=None):
    """Production key locations: exactly `count` lines on stdin, signing key 1 first.

    Never argv, never the environment. On a terminal every line is prompted for
    on stderr; from a pipe (scripts/release.sh) exactly `count` lines and nothing
    else must arrive. Messages never contain a location.
    """
    if not _integer(count, 1, MAX_SIGNERS):
        raise ValueError("use 1 to 6 signing keys")
    if stream is None:
        if sys.stdin is None:
            raise ValueError("key locations are read from stdin, which is closed")
        stream = sys.stdin.buffer
    if interactive is None:
        interactive = stream.isatty()
    locations = []
    for index in range(1, count + 1):
        if interactive:
            sys.stderr.write(f"Location of signing key {index} of {count} (key file or security-key handle; "
                             "a path, not a secret): ")
            sys.stderr.flush()
        line = stream.readline(MAX_LOCATION_BYTES + 1)
        if not line.endswith(b"\n"):
            raise ValueError(f"signing key {index}: no complete location line on stdin "
                             "(one line per key, signing key 1 first)")
        value = line[:-1]
        if not value or any(byte in value for byte in b"\0\r"):
            raise ValueError(f"signing key {index}: empty or malformed location")
        location = Path(os.path.abspath(os.fsdecode(value)))
        if not location.is_file():
            raise ValueError(f"signing key {index} is not a file")
        locations.append(location)
    if not interactive and stream.read(1):
        raise ValueError(f"stdin carries more than the {count} expected key location line(s)")
    if len(set(locations)) != len(locations):
        raise ValueError("use distinct signing keys")
    return locations


@contextlib.contextmanager
def terminal_stdin():
    """The controlling terminal as a signer's stdin (ssh-keygen and minisign ask for the
    PIN / passphrase there; stdin of this process is the key-location pipe); /dev/null without one."""
    try:
        descriptor = os.open("/dev/tty", os.O_RDWR | getattr(os, "O_CLOEXEC", 0))
    except OSError:
        descriptor = None
    try:
        yield subprocess.DEVNULL if descriptor is None else descriptor
    finally:
        if descriptor is not None:
            os.close(descriptor)


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


def _edwards_point(encoded):
    """RFC 8032 §5.1.3 decoding of an Ed25519 point; ValueError unless canonical and on the curve."""
    if len(encoded) != 32:
        raise ValueError("an Ed25519 point has 32 bytes")
    y = int.from_bytes(encoded, "little")
    sign, y = y >> 255, y & ((1 << 255) - 1)
    if y >= _FIELD:
        raise ValueError("non-canonical Ed25519 point")
    u, v = (y * y - 1) % _FIELD, (_CURVE_D * y * y + 1) % _FIELD
    x2 = u * pow(v, _FIELD - 2, _FIELD) % _FIELD
    if x2 == 0:
        if sign:
            raise ValueError("non-canonical Ed25519 point")
        return 0, y
    x = pow(x2, (_FIELD + 3) // 8, _FIELD)
    if (x * x - x2) % _FIELD:
        x = x * _SQRT_MINUS_ONE % _FIELD
    if (x * x - x2) % _FIELD:
        raise ValueError("not an Ed25519 curve point")
    if x & 1 != sign:
        x = _FIELD - x
    return x, y


def _edwards_add(first, second):
    """Complete twisted Edwards addition (a = -1) in affine coordinates."""
    (x1, y1), (x2, y2) = first, second
    t = _CURVE_D * x1 * x2 * y1 * y2 % _FIELD
    x3 = (x1 * y2 + y1 * x2) * pow((1 + t) % _FIELD, _FIELD - 2, _FIELD) % _FIELD
    y3 = (y1 * y2 + x1 * x2) * pow((1 - t) % _FIELD, _FIELD - 2, _FIELD) % _FIELD
    return x3, y3


def is_small_order(encoded):
    """ed25519-dalek is_weak / is_small_order: 8·P is the identity. Undecodable points raise."""
    point = _edwards_point(encoded)
    for _ in range(3):
        point = _edwards_add(point, point)
    return point == (0, 1)


def check_signature_shape(public, inner, key):
    """Reject before any cryptography what sshsig.rs (SshSignature::verify, validate_key) rejects:
    key and signature algorithm ssh-ed25519 or sk-ssh-ed25519@openssh.com, matching each other and
    the registered key; the role application bound in an sk key blob; a 32-byte key that is no
    small-order point; a 64-byte signature with canonical S and a non-small-order R;
    security-key flags exactly UP|UV plus a 4-byte counter; no trailing bytes anywhere."""
    name, offset = _ssh_string(public, 0)
    raw, offset = _ssh_string(public, offset)
    algorithm, application = key.get("algorithm"), key.get("application")
    if len(raw) != 32:
        raise ValueError("root key is not a 32-byte Ed25519 key")
    if name == ED25519 and algorithm == "ed25519" and application is None:
        pass
    elif (name == SK_ED25519 and algorithm == "sk_ed25519"
          and application in (ROOT_SK_APPLICATION, TARGET_SK_APPLICATION)):
        bound, offset = _ssh_string(public, offset)
        if bound != application.encode("ascii"):
            raise ValueError("security-key blob does not bind the registered application")
    else:
        raise ValueError("root key is not ssh-ed25519 or sk-ssh-ed25519@openssh.com with its registered "
                         "algorithm and application (the verifier accepts nothing else)")
    if offset != len(public):
        raise ValueError("trailing bytes after the root key")
    if is_small_order(raw):
        raise ValueError("root key is a weak (small-order) Ed25519 point")
    signed_name, offset = _ssh_string(inner, 0)
    signature, offset = _ssh_string(inner, offset)
    if signed_name != name or len(signature) != 64:
        raise ValueError("signature algorithm or length is not what the verifier accepts for this key")
    if name == SK_ED25519:
        if offset + 5 > len(inner) or inner[offset] != REQUIRED_SK_FLAGS:
            raise ValueError("security-key signature flags must be exactly user presence + user verification")
        offset += 5
    if offset != len(inner):
        raise ValueError("trailing bytes after the signature")
    if int.from_bytes(signature[32:], "little") >= ED25519_GROUP_ORDER or is_small_order(signature[:32]):
        raise ValueError("non-canonical or small-order Ed25519 signature")


def ssh_keygen_verifier(ssh_keygen):
    """Production cryptographic check: `ssh-keygen -Y verify` over the exact bytes and namespace,
    against an allowed-signers file in a private temporary directory that lists ONLY the root key
    whose key id was computed, under the principal <key id hex>."""
    tool = resolve_tool(ssh_keygen, "--ssh-keygen")

    def verify(armor, key_id, public, namespace, payload):
        algorithm = _ssh_string(public, 0)[0].decode("ascii")
        principal = key_id.hex()
        with tempfile.TemporaryDirectory(prefix="duduclaw-verify-") as temporary:
            directory = Path(temporary)
            directory.chmod(0o700)
            allowed = directory / "allowed_signers"
            allowed.write_text(f'{principal} namespaces="{namespace}" {algorithm} '
                               f'{base64.b64encode(public).decode("ascii")}\n', encoding="ascii")
            signature = directory / "signature"
            signature.write_text(armor, encoding="ascii")
            result = subprocess.run([tool, "-Y", "verify", "-f", str(allowed), "-I", principal, "-n", namespace,
                                     "-s", str(signature)], input=payload, env=child_environment(),
                                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        if result.returncode != 0:
            detail = result.stderr.decode("utf-8", "replace").strip().splitlines()
            raise ValueError(f"the signature of key {principal} does not verify (ssh-keygen -Y verify"
                             + (f": {detail[-1]}" if detail else "") + ")")
    return verify


def isolation_test_signer_verifier(test_signer, root_bytes, kind):
    """TEST mode: `<test signer> verify`, i.e. the crate's production SshSignature::verify. Never skipped."""
    tool = resolve_tool(os.path.abspath(test_signer), "--test-signer")

    def verify(armor, key_id, public, namespace, payload):
        if NAMESPACES[kind] != namespace:
            raise ValueError("namespace does not belong to this document kind")
        with tempfile.TemporaryDirectory(prefix="duduclaw-verify-") as temporary:
            directory = Path(temporary)
            directory.chmod(0o700)
            (directory / "root.json").write_bytes(root_bytes)
            (directory / "document").write_bytes(payload)
            (directory / "signature").write_text(armor, encoding="ascii")
            result = subprocess.run([tool, "verify", str(directory / "root.json"), kind, str(directory / "document"),
                                     str(directory / "signature"), key_id.hex()], env=child_environment(),
                                    stdin=subprocess.DEVNULL, capture_output=True)
        if result.returncode != 0:
            raise ValueError(f"the TEST signature of root key {key_id.hex()} does not verify (test signer verify)")
    return verify


def parse_signature(armor, namespace):
    """(public key blob, inner signature blob) of an armored SSHSIG, parsed as strictly as sshsig.rs."""
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
    signature, offset = _ssh_string(blob, offset)
    if (offset != len(blob) or not 0 < len(public) <= MAX_KEY_BLOB_BYTES or reserved
            or hash_algorithm != b"sha512" or signed_namespace != namespace.encode("ascii")):
        raise ValueError("SSH signature has the wrong namespace, hash algorithm or shape")
    return public, signature


def _byte_array(value, length=None):
    if (not isinstance(value, list) or (length is not None and len(value) != length)
            or not all(_integer(item, 0, 255) for item in value)):
        raise ValueError("root metadata byte arrays must be lists of 0..255")
    return bytes(value)


def root_signers(root_bytes):
    """({key_id: {"blob", "algorithm", "application"}}, revoked key ids, root_threshold) of canonical root metadata."""
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
        roots[key_id] = {"blob": blob, "algorithm": key.get("algorithm"), "application": key.get("application")}
    revoked = document.get("revoked_key_ids")
    if not isinstance(revoked, list):
        raise ValueError("invalid revoked_key_ids")
    revoked = {_byte_array(item, 32) for item in revoked}
    threshold = document.get("root_threshold")
    if not _integer(threshold, 2, len(roots)):
        raise ValueError("root_threshold must be between 2 and the number of root keys")
    return roots, revoked, threshold


def check_root_threshold(signatures, root_bytes, namespace, payload, verify):
    """Same rules as duduclaw-updated trust::signatures/reject_unknown/threshold: every
    signer is identified by the key embedded in its signature, never by a path or handle,
    and every signature must verify over `payload` (the exact canonical bytes) under
    `namespace` with `verify` (ssh_keygen_verifier or isolation_test_signer_verifier),
    which is mandatory. Returns the distinct signer key ids (hex)."""
    roots, revoked, threshold = root_signers(root_bytes)
    if not isinstance(payload, bytes) or not callable(verify):
        raise ValueError("signatures are accepted only after cryptographic verification over the exact bytes")
    if not isinstance(signatures, list) or not 1 <= len(signatures) <= MAX_SIGNERS:
        raise ValueError("expected 1 to 6 signatures")
    seen = set()
    checked = []
    for armor in signatures:
        public, inner = parse_signature(armor, namespace)
        key_id = hashlib.sha256(public).digest()
        if key_id in seen:
            raise ValueError("two signatures were made by the same key (another path, link or handle "
                             "to one key is not a second signer)")
        seen.add(key_id)
        key = roots.get(key_id)
        if key is None or key["blob"] != public:
            raise ValueError("a signature was made by a key that is not a root key of the root metadata")
        if key_id in revoked:
            raise ValueError("a signature was made by a revoked root key")
        check_signature_shape(public, inner, key)
        checked.append((armor, key_id, public))
    if len(seen) < threshold:
        raise ValueError(f"{len(seen)} distinct root key(s) do not meet the root threshold of {threshold}")
    for armor, key_id, public in checked:
        verify(armor, key_id, public, namespace, payload)
    return sorted(key_id.hex() for key_id in seen)


def target_signer(root_bytes):
    """(key id, key) of the target_key of canonical root metadata (validated by root_signers):
    its key id is the SHA-256 of its own blob, it is not revoked, and an sk key is bound to the
    target application."""
    _, revoked, _ = root_signers(root_bytes)
    key = json.loads(root_bytes).get("target_key")
    if not isinstance(key, dict):
        raise ValueError("root metadata has no target key")
    blob, key_id = _byte_array(key.get("public_blob")), _byte_array(key.get("key_id"), 32)
    if not 0 < len(blob) <= MAX_KEY_BLOB_BYTES or hashlib.sha256(blob).digest() != key_id:
        raise ValueError("the target key id must be the SHA-256 of its own public key blob")
    if key_id in revoked:
        raise ValueError("the target key of the root metadata is revoked")
    application = key.get("application")
    if application not in (None, TARGET_SK_APPLICATION):
        raise ValueError("the target key is not bound to the target application")
    return key_id, {"blob": blob, "algorithm": key.get("algorithm"), "application": application}


def check_target_signature(signatures, root_bytes, namespace, payload, verify):
    """A release/runtime manifest carries exactly one signature, made by the target_key of the
    (pinned) root metadata: the key embedded in the signature is that key, the signature has the
    shape the Rust verifier accepts, and `verify` (ssh_keygen_verifier: an allowed-signers file
    with only that key; or isolation_test_signer_verifier) accepts it over `payload`, which is
    mandatory. Returns the target key id (hex)."""
    key_id, key = target_signer(root_bytes)
    if not isinstance(payload, bytes) or not callable(verify):
        raise ValueError("signatures are accepted only after cryptographic verification over the exact bytes")
    if not isinstance(signatures, list) or len(signatures) != 1:
        raise ValueError("a release manifest carries exactly one target-key signature")
    public, inner = parse_signature(signatures[0], namespace)
    if hashlib.sha256(public).digest() != key_id or public != key["blob"]:
        raise ValueError("the manifest was signed by a key that is not the target key of the pinned root metadata "
                         "(was the wrong key selected?)")
    check_signature_shape(public, inner, key)
    verify(signatures[0], key_id, public, namespace, payload)
    return key_id.hex()


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


def check_bound_artifacts(manifest_path, document, directory):
    """--check-target --artifacts-dir: every artifact the signed manifest binds is
    <directory>/<prefix><id> (prefix: the manifest file name without "manifest.json", the names
    the packager publishes) with exactly the digest and size the manifest states. The binding is
    recomputed by bind_artifacts, the signing-time code, and must give the signed bytes."""
    name = Path(manifest_path).name
    if not name.endswith("manifest.json"):
        raise ValueError("--artifacts-dir needs a manifest named <prefix>manifest.json")
    rebound = json.loads(document, object_pairs_hook=unique_object, parse_float=reject_number,
                         parse_constant=reject_number)
    try:
        if not isinstance(rebound, dict):
            raise TypeError("not an object")
        bind_artifacts(rebound, Path(directory), name[:-len("manifest.json")])
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError("malformed artifact list in the manifest") from error
    if canonical_bytes(rebound) != document:
        raise ValueError("an artifact differs from the digest or size the signed manifest states")


def check_written_threshold(parser, args):
    """--check-threshold: verify a written document + signatures; never signs or writes. With
    --kind bundle and --bundle, the tar must also be exactly the one authorized (bundle_authorization:
    name, digest and size)."""
    if args.kind not in THRESHOLD_KINDS:
        parser.error("--check-threshold applies to --kind root or --kind bundle")
    if any(value is not None for value in (args.output, args.key, args.key_count, args.template, args.artifacts_dir,
                                           args.expires_at_unix, args.minisign_sign,
                                           args.trusted_comment, args.ssh_sk_helper, args.check_target)):
        parser.error("--check-threshold takes only --kind, --signatures, --root-metadata, --root-sha256, --bundle "
                     "(--kind bundle) and the verifier (--ssh-keygen, or --test-signer for TEST signatures)")
    if args.bundle is not None and args.kind != "bundle":
        parser.error("--check-threshold --bundle applies to --kind bundle")
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
        verify = (isolation_test_signer_verifier(args.test_signer, root_bytes, args.kind)
                  if args.test_signer is not None else ssh_keygen_verifier(args.ssh_keygen))
        signers = check_root_threshold(signatures, root_bytes, NAMESPACES[args.kind], document, verify)
        if args.bundle is not None:
            expires = parsed.get("expires_at_unix") if isinstance(parsed, dict) else None
            if canonical_bytes(bundle_authorization(args.bundle, expires)) != document:
                raise ValueError("the bundle differs from the digest, size, version or architecture its "
                                 "authorization states")
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f"root threshold met by {len(signers)} distinct root keys; every signature verified"
          + ("; the bundle is the authorized one" if args.bundle is not None else ""))


def check_written_target(parser, args):
    """--check-target: verify a written release/runtime manifest and its signature against the
    target key of --root-metadata (scripts/release.sh, before the upload list and again right
    before the upload); never signs. With --artifacts-dir, also every bound artifact
    (check_bound_artifacts)."""
    if args.kind not in TARGET_KINDS:
        parser.error("--check-target applies to --kind release or --kind runtime")
    if any(value is not None for value in (args.output, args.key, args.key_count, args.template,
                                           args.bundle, args.expires_at_unix, args.minisign_sign,
                                           args.trusted_comment, args.ssh_sk_helper, args.check_threshold)):
        parser.error("--check-target takes only --kind, --signatures, --root-metadata, --root-sha256, "
                     "--artifacts-dir and the verifier (--ssh-keygen, or --test-signer for TEST signatures)")
    if args.root_metadata is None:
        parser.error("--check-target requires --root-metadata (the pinned root.json naming the target key)")
    try:
        document = read_bounded(args.check_target, MAX_METADATA_BYTES)
        parsed = json.loads(document, object_pairs_hook=unique_object, parse_float=reject_number,
                            parse_constant=reject_number)
        if canonical_bytes(parsed) != document:
            raise ValueError("the manifest is not canonical")
        signatures = json.loads(read_bounded(args.signatures or Path(str(args.check_target) + ".signatures.json"),
                                             MAX_METADATA_BYTES))
        root_bytes = read_root_metadata(args.root_metadata, args.root_sha256)
        verify = (isolation_test_signer_verifier(args.test_signer, root_bytes, args.kind)
                  if args.test_signer is not None else ssh_keygen_verifier(args.ssh_keygen))
        key_id = check_target_signature(signatures, root_bytes, NAMESPACES[args.kind], document, verify)
        if args.artifacts_dir is not None:
            check_bound_artifacts(args.check_target, document, args.artifacts_dir)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f"target key {key_id}: the manifest signature verified"
          + ("; every bound artifact has the signed digest and size" if args.artifacts_dir is not None else ""))


def minisign_sign(parser, args):
    """--minisign-sign: the legacy release key signs one file (scripts/release.sh, migration bundle).

    The key location arrives on stdin like every production key location; minisign
    sees only the neutral link, the child environment and the terminal as stdin
    (it reads its password there). Nothing is written unless minisign succeeds;
    an existing output is never overwritten. The caller verifies the result.
    """
    if any(value is not None for value in (args.template, args.artifacts_dir, args.bundle, args.expires_at_unix,
                                           args.key, args.key_count, args.root_metadata, args.root_sha256,
                                           args.signatures, args.test_signer, args.ssh_sk_helper, args.check_target)):
        parser.error("--minisign-sign takes only --trusted-comment, --output and --minisign")
    comment = args.trusted_comment
    if args.output is None or comment is None:
        parser.error("--minisign-sign requires --trusted-comment and --output")
    if not comment or len(comment) > 1024 or not comment.isprintable():
        parser.error("--trusted-comment must be one printable line")
    try:
        refuse_key_variables()
        tool = resolve_tool(args.minisign, "--minisign")
        source = os.lstat(args.minisign_sign)
        if not stat.S_ISREG(source.st_mode) or source.st_size == 0:
            raise ValueError("--minisign-sign must name a nonempty regular file")
        if args.output.exists() or args.output.is_symlink():
            raise ValueError("output already exists")
        [key] = read_key_locations(1)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    with tempfile.TemporaryDirectory(prefix="duduclaw-minisign-") as temporary:
        staged = Path(temporary) / "signature.minisig"
        try:
            with neutral_key_link(key) as link, terminal_stdin() as terminal:
                subprocess.run([tool, "-S", "-s", str(link), "-m", str(args.minisign_sign), "-t", comment,
                                "-x", str(staged)], check=True, env=child_environment(), stdin=terminal)
        except (OSError, subprocess.CalledProcessError):
            parser.error("minisign -S failed; nothing was written")
        try:
            signature = staged.read_bytes()
        except OSError:
            signature = b""
        if not signature:
            parser.error("minisign wrote no signature; nothing was written")
        with args.output.open("xb") as output:
            output.write(signature)


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
                        help="production: number of signing keys (default 1; root/bundle: 2 or 3); their "
                             "locations are read from stdin, one per line, key 1 first")
    parser.add_argument("--root-metadata", type=Path,
                        help="--kind bundle: the pinned canonical root.json whose root keys must sign; --kind "
                             "release/runtime: the pinned root.json whose target_key must have made the signature")
    parser.add_argument("--root-sha256", help="expected SHA-256 (bootstrap pin) of the root metadata")
    parser.add_argument("--check-threshold", type=Path,
                        help="verify an already written root/bundle document and its signatures; signs nothing")
    parser.add_argument("--check-target", type=Path,
                        help="verify an already written release/runtime manifest against the target key of "
                             "--root-metadata; signs nothing")
    parser.add_argument("--signatures", type=Path,
                        help="with --check-threshold/--check-target: signatures JSON "
                             "(default <document>.signatures.json)")
    parser.add_argument("--ssh-keygen", default="ssh-keygen",
                        help="absolute path (scripts/release.sh passes one), or a name looked up only in "
                             + ", ".join(TOOL_DIRECTORIES) + "; use Homebrew OpenSSH for FIDO2 on macOS. "
                             "Signs, and verifies every root-threshold signature (-Y verify)")
    parser.add_argument("--ssh-sk-helper", type=Path,
                        help="absolute path of the audited libexec/ssh-sk-helper of the selected ssh-keygen's "
                             "installation (scripts/release.sh passes it); default: found next to --ssh-keygen. "
                             "Given to ssh-keygen -Y sign only, as SSH_SK_HELPER")
    parser.add_argument("--test-signer", type=Path,
                        help="TEST-ONLY: duduclaw-isolation-test-signer instead of ssh-keygen (test key set only); "
                             "its `verify` command checks root-threshold signatures")
    parser.add_argument("--minisign-sign", type=Path,
                        help="legacy release key: minisign-sign this file (key location on stdin)")
    parser.add_argument("--trusted-comment", help="with --minisign-sign: the exact trusted comment")
    parser.add_argument("--minisign", default="minisign",
                        help="with --minisign-sign: absolute path, or a name looked up like --ssh-keygen")
    args = parser.parse_args()
    if args.test_signer is not None and (args.ssh_keygen != "ssh-keygen" or args.ssh_sk_helper is not None):
        parser.error("--test-signer and --ssh-keygen/--ssh-sk-helper are mutually exclusive")
    if args.minisign_sign is not None:
        if args.check_threshold is not None or args.check_target is not None:
            parser.error("--minisign-sign and --check-threshold/--check-target are mutually exclusive")
        minisign_sign(parser, args)
        return
    if args.trusted_comment is not None or args.minisign != "minisign":
        parser.error("--trusted-comment/--minisign belong to --minisign-sign")
    if args.check_threshold is not None:
        check_written_threshold(parser, args)
        return
    if args.check_target is not None:
        check_written_target(parser, args)
        return
    if args.signatures is not None:
        parser.error("--signatures belongs to --check-threshold/--check-target")
    if args.output is None:
        parser.error("--output is required")
    if args.test_signer is None:
        if args.key:
            parser.error("production signing never takes --key: key locations are read from stdin, one per "
                         "line, because argv is visible to other local processes (--key is only for "
                         "--test-signer)")
        count = 1 if args.key_count is None else args.key_count
    else:
        if args.key_count is not None:
            parser.error("--key-count is for production signing; --test-signer takes --key")
        if not args.key or len(set(args.key)) != len(args.key):
            parser.error("--test-signer needs distinct --key files")
        count = len(args.key)
    if not _integer(count, 1, MAX_SIGNERS):
        parser.error("use 1 to 6 distinct signing keys")
    if args.kind not in ("bundle", *TARGET_KINDS) and (args.root_metadata is not None or args.root_sha256 is not None):
        parser.error("--root-metadata/--root-sha256 belong to --kind bundle (root threshold) or --kind release/"
                     "runtime (target key); a root is checked against itself")
    # --kind release/runtime with --root-metadata: the target key of that root must sign (and verify).
    target_root = None
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
            if args.root_sha256 is not None and args.root_metadata is None:
                parser.error("--root-sha256 pins --root-metadata; give both")
            if args.root_metadata is not None:
                if count != 1:
                    parser.error("a release manifest is signed by the one target key: --key-count 1")
                try:
                    target_root = read_root_metadata(args.root_metadata, args.root_sha256)
                    target_signer(target_root)
                except (OSError, ValueError) as error:
                    parser.error(str(error))
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
    # Every refusal above happens before a key location is read or a key is touched.
    try:
        if args.test_signer is None:
            refuse_key_variables()
            args.ssh_keygen = resolve_tool(args.ssh_keygen, "--ssh-keygen")
            if args.ssh_sk_helper is not None:
                args.ssh_sk_helper = resolve_sk_helper(args.ssh_keygen, args.ssh_sk_helper)
        verify = None
        if args.kind in THRESHOLD_KINDS or target_root is not None:
            checked_root = root_bytes if args.kind in THRESHOLD_KINDS else target_root
            verify = (isolation_test_signer_verifier(args.test_signer, checked_root, args.kind)
                      if args.test_signer is not None else ssh_keygen_verifier(args.ssh_keygen))
        keys = args.key if args.test_signer is not None else read_key_locations(count)
        # FIDO2: a security key (or a key whose type its .pub does not show) needs the ssh-sk-helper
        # of the selected ssh-keygen's installation; no other helper is ever used.
        helpers = []
        for index, key in enumerate(keys if args.test_signer is None else (), 1):
            helper = None
            if may_be_security_key(key):
                helper = args.ssh_sk_helper or resolve_sk_helper(args.ssh_keygen)
                if helper is None:
                    raise ValueError(
                        f"signing key {index} is a security key (or has no readable .pub showing its type), and the "
                        f"selected ssh-keygen has no ssh-sk-helper in its installation ("
                        f"{' or '.join(sk_helper_candidates(args.ssh_keygen))}); select an ssh-keygen whose "
                        "installation has one (--ssh-keygen). No other helper is ever used.")
            helpers.append(helper)
    except ValueError as error:
        parser.error(str(error))
    # No output is published until every signer succeeds. Existing outputs are
    # never overwritten; release operators publish the resulting pair together.
    with tempfile.TemporaryDirectory(prefix="duduclaw-sign-") as temporary:
        stage = Path(temporary) / "metadata.json"
        stage.write_bytes(payload)
        signatures = []
        for index, key in enumerate(keys):
            if args.test_signer is not None:
                subprocess.run(signer_command(args, key, stage), check=True, env=child_environment(),
                               stdin=subprocess.DEVNULL)
            else:
                with neutral_key_link(key) as link, terminal_stdin() as terminal:
                    subprocess.run(signer_command(args, link, stage), check=True,
                                   env=signing_environment(link, helpers[index]), stdin=terminal)
            detached = Path(str(stage) + ".sig")
            signatures.append(detached.read_text(encoding="ascii"))
            detached.unlink()
        if target_root is not None:
            # A wrong but valid key (e.g. a root key, or another security key) is refused here,
            # before anything is written: only the pinned root's target_key may sign a manifest.
            try:
                check_target_signature(signatures, target_root, NAMESPACES[args.kind], payload, verify)
            except ValueError as error:
                parser.error(f"{error}; nothing was written")
        if args.kind in THRESHOLD_KINDS:
            # R2-5: signers are the keys embedded in the signatures, checked against the
            # root metadata and verified cryptographically over `payload` before anything
            # is written (so nothing is left to delete).
            try:
                check_root_threshold(signatures, root_bytes, NAMESPACES[args.kind], payload, verify)
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
