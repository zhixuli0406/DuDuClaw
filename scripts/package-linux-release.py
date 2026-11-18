#!/usr/bin/env python3
"""Build/package the complete native Linux release; offline signing is optional.

The caller builds web/dist first. Without --signing-key this produces the normal
tarball and SHA-256 only. Offline operators may supply a reviewed F release
template and their external target key; no key is copied and CI cannot sign.

The root bootstrap verifier (duduclaw-bootstrap) is packaged only with an
explicit production root pin: --bootstrap-root-sha256 <hex>, or
--bootstrap-root-pin-file release/bootstrap-root.sha256 (the public file added
after the key ceremony; while it is absent there is no production pin, which is
the documented way to build an archive WITHOUT the verifier: it is then not
built, not archived, and both a SKIP line and the final summary say so). With a
pin, a test-root build environment is refused, and create_archive() itself runs
one uninterruptible flow for the verifier: read the staged file ONCE into an
anonymous memfd, seal it, check the seals, hash it, require an ELF for the
target, execute that descriptor twice (it must report exactly `PRODUCTION <pin>`
and this release's version), write the archive member from the same descriptor,
re-check seals and digest, close it. The archived bytes are the executed bytes
whatever happens to any pathname. There is no proof, token or descriptor to hand
around: callers name a path (or a released archive) plus the expected pin,
version and target, and get a plain report afterwards. The flow is bound to the
target: it refuses unless this host is Linux of the target's architecture, and
the archive it writes is named for that target. There is no attestation
alternative: a verifier this host cannot execute (foreign architecture, exec
format error, missing loader, no memfd) is refused with a non-zero exit, never
skipped. Package each architecture that ships the verifier on a native Linux
host of it.

Cross-architecture release (CI): build with --build-only --defer-bootstrap-probe
on the build host (the verifier is built with the pin but not executed), then
--skip-build --export-binaries <dir> writes the snapshot plus SHA256SUMS; the
native packaging host runs --skip-build --binary-dir <dir> --binary-sums-sha256
<sha256 of SHA256SUMS>, which re-hashes every byte it packages and probes the
verifier natively. --verify-archive <tarball> re-executes the verifier inside a
finished archive through the same sealed flow, from the member bytes (never an
extracted path), or proves its absence when no pin is configured.
--defer-bootstrap-probe exists only for --build-only, which never packages.

Offline isolation release (the ceremony, started only as scripts/isolation-sign,
which runs scripts/release.sh isolation-sign):
  --extract-bootstrap <tarball>     copy the released verifier out as duduclaw-bootstrap-linux-<arch>
  --from-release-archive <tarball>  sign the isolation manifest over the binaries of
                                    the released tarball (no new tarball), optionally
                                    with the bootstrap inbox (--bootstrap-root ...);
                                    --signing-key-stdin reads the target key location
                                    from stdin (one line), never argv or the environment;
                                    --target-root <root.json> [--target-root-sha256 <pin>]
                                    (default: --bootstrap-root): nothing is written unless
                                    the signature verifies against that root's target_key
  --migration-bundle-from <inbox>   write the plain-ustar migration bundle (plan §3.2)
                                    duduclaw-isolated-linux-<arch>-<x.y.z>.tar

TEST-ONLY: duduclaw-isolation-test-signer is never built, listed or packaged here;
a build command that names the `test-signer` feature or that binary is refused.
--test-signer is accepted only with --from-release-archive and without any root
pin (scripts/isolation-test-keys.sh), never for a production artifact.
"""
import argparse
import collections
import contextlib
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import select
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-unknown-linux-gnu": ("x86_64", "duduclaw-linux-x64"),
    "aarch64-unknown-linux-gnu": ("aarch64", "duduclaw-linux-arm64"),
}
# Select explicit production bins; --bins would also select root-test fixtures
# and, with the `test-signer` feature, the TEST-ONLY signer (never listed here).
BIN_PACKAGES = {
    "duduclaw": "duduclaw-cli",
    "duduclaw-spawnd": "duduclaw-spawnd",
    "duduclaw-run": "duduclaw-run",
    # R7a isolation self-test, same crate and build as the trampoline.
    "duduclaw-run-selftest": "duduclaw-run",
    # R7c SSH denial probe (template unit duduclaw-isolation-probe@.service).
    "duduclaw-isolation-probe": "duduclaw-isolation",
    "duduclaw-egress": "duduclaw-egress",
    "duduclaw-updated": "duduclaw-updated",
    # Root transition verifier (plan R1); customers compare its published SHA-256.
    "duduclaw-bootstrap": "duduclaw-updated",
    "duduclaw-parse-worker": "duduclaw-parse-worker",
    "duduclaw-import": "duduclaw-import",
    "duduclaw-migrate": "duduclaw-migration",
    "duduclaw-mcp-bridge": "duduclaw-mcp-boundary",
    "duduclaw-api-worker": "duduclaw-api-worker",
    "duduclaw-runtime-adapter": "duduclaw-api-worker",
}
INSTALL_BINARIES = {
    "gateway": "duduclaw", "spawnd": "duduclaw-spawnd",
    "trampoline": "duduclaw-run", "trampoline_selftest": "duduclaw-run-selftest",
    "isolation_probe": "duduclaw-isolation-probe", "egress": "duduclaw-egress",
    "egress_forwarder": "duduclaw-egress", "mcp_bridge": "duduclaw-mcp-bridge",
    "parse_worker": "duduclaw-parse-worker", "importer": "duduclaw-import",
    "migration": "duduclaw-migrate", "update_slot": "duduclaw-updated",
}
REQUIRED_TARGETS = set(INSTALL_BINARIES)
# Fixed argv selects the launcher/forwarder entrypoint of these same ELFs.
ARCHIVE_ALIASES = {
    "duduclaw-egress-forwarder": "duduclaw-egress",
    "duduclaw-updated-launch": "duduclaw-updated",
}
# Kept below the generic updater's 768 MiB cap (duduclaw-gateway updater.rs).
MAX_BINARY_BYTES = 512 * 1024 * 1024
MAX_RELEASE_ARCHIVE_BYTES = 4 * 1024 * 1024 * 1024

# Root bootstrap verifier (plan R1 §4.1, R2 §4.2). Its root pin is compiled in
# through this variable; a `--cfg duduclaw_test_root` build reports TEST.
BOOTSTRAP_BINARY = "duduclaw-bootstrap"
BOOTSTRAP_PIN_VARIABLE = "DUDUCLAW_BOOTSTRAP_ROOT_SHA256"
BOOTSTRAP_TEST_KEY_VARIABLE = "DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY"
TEST_ROOT_CFG = "duduclaw_test_root"
# The standalone verifier asset of an isolation release (scripts/release.sh isolation-sign).
BOOTSTRAP_ASSET_PREFIX = "duduclaw-bootstrap-linux-"
# cross runs Cargo in a container and forwards only listed variables
# (Cross.toml [build.env] passthrough, or this environment variable).
CROSS_PASSTHROUGH_VARIABLE = "CROSS_BUILD_ENV_PASSTHROUGH"
MAX_PIN_FILE_BYTES = 128
# TEST-ONLY signer (crates/duduclaw-updated, feature `test-signer`): never built
# or packaged by this script (plan R2 §4.2, test-key leak layer 1).
TEST_SIGNER_BINARY = "duduclaw-isolation-test-signer"
TEST_SIGNER_FEATURE = "test-signer"
# Flag variables Cargo hands to rustc, with their separators (Cargo reference,
# "Environment variables"); CARGO_TARGET_<TRIPLE>_RUSTFLAGS is checked as well.
RUSTFLAGS_VARIABLES = (("CARGO_ENCODED_RUSTFLAGS", "\x1f"), ("RUSTFLAGS", None), ("CARGO_BUILD_RUSTFLAGS", None))
MAX_PROBE_OUTPUT_BYTES = 256
PROBE_TIMEOUT_SECONDS = 30
# Linux uapi values (linux/memfd.h, linux/fcntl.h) for Pythons that lack the names.
MFD_CLOEXEC = getattr(os, "MFD_CLOEXEC", 0x0001)
MFD_ALLOW_SEALING = getattr(os, "MFD_ALLOW_SEALING", 0x0002)
MFD_EXEC = getattr(os, "MFD_EXEC", 0x0010)  # Linux 6.3+; older kernels reject it with EINVAL
F_DUPFD_CLOEXEC = getattr(fcntl, "F_DUPFD_CLOEXEC", 1030)
F_ADD_SEALS = getattr(fcntl, "F_ADD_SEALS", 1033)
F_GET_SEALS = getattr(fcntl, "F_GET_SEALS", 1034)
F_SEAL_SEAL = getattr(fcntl, "F_SEAL_SEAL", 0x0001)
F_SEAL_SHRINK = getattr(fcntl, "F_SEAL_SHRINK", 0x0002)
F_SEAL_GROW = getattr(fcntl, "F_SEAL_GROW", 0x0004)
F_SEAL_WRITE = getattr(fcntl, "F_SEAL_WRITE", 0x0008)
BOOTSTRAP_SEALS = F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE
ELF_MACHINES = {"x86_64": 62, "aarch64": 183}  # e_machine: EM_X86_64, EM_AARCH64
OMITTED_BOOTSTRAP_LINE = ("SKIP duduclaw-bootstrap: no production root pin configured "
                          "(--bootstrap-root-sha256 / release/bootstrap-root.sha256); "
                          "the root bootstrap verifier is not packaged.")
DEFERRED_BOOTSTRAP_LINE = ("DEFERRED duduclaw-bootstrap: built with the production root pin but NOT executed "
                           "on this host; it can only be packaged by a native host that executes it.")
# Hand-off of cross-built binaries to a native packaging host.
TRANSFER_SUMS = "SHA256SUMS"
MAX_TRANSFER_SUMS_BYTES = 64 * 1024
MAX_SIDECAR_BYTES = 512
# Migration bundle (plan §3.2): plain ustar with exactly these metadata members.
BUNDLE_METADATA = ("root.json", "root.json.signatures.json", "manifest.json", "manifest.json.signatures.json")
MAX_BUNDLE_BYTES = 2 * 1024 * 1024 * 1024
# Fields the repository base template must leave to render_release_template.
PER_RELEASE_FIELDS = ("version", "arch", "serial", "created_at_unix", "expires_at_unix")
MAX_VALID_DAYS = 1830
IDENTIFIER = re.compile(r"[A-Za-z0-9_.-]{1,128}")


def workspace_version():
    # Only this stable, literal workspace field is consumed; avoid requiring
    # Python 3.11's tomllib on supported Ubuntu 22.04 build hosts.
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = re.search(r"(?ms)^\[workspace\.package\][ \t]*\n(.*?)(?=^\[|\Z)", text)
    version = re.search(r'^version[ \t]*=[ \t]*"([0-9]+\.[0-9]+\.[0-9]+)"[ \t]*(?:#.*)?$',
                        section[1] if section else "", re.MULTILINE)
    if version is None:
        raise ValueError("workspace has no literal stable version")
    return dict(zip(("major", "minor", "patch"), map(int, version[1].split("."))))


def version_text(version):
    return f"{version['major']}.{version['minor']}.{version['patch']}"


# Production key locations travel only on stdin: --signing-key-stdin reads the target key
# location as one line (the scripts/isolation-sign ceremony pipes it), and sign_release hands
# it to sign-isolation-manifest.py on that child's stdin. Never argv, never the environment.


def load_signer():
    spec = importlib.util.spec_from_file_location("isolation_signer", ROOT / "scripts/sign-isolation-manifest.py")
    signer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(signer)
    return signer


def check_release_binary_set(packages=None):
    """The release binary set never names the TEST-ONLY signer or its feature."""
    packages = BIN_PACKAGES if packages is None else packages
    for name, package in packages.items():
        if TEST_SIGNER_BINARY in name or TEST_SIGNER_FEATURE in name or TEST_SIGNER_FEATURE in package:
            raise ValueError(f"{name} is a TEST-ONLY binary and can never be a release binary")


def archive_binaries(include_bootstrap):
    """Production binaries for this run; the verifier only with a verified pin."""
    return [name for name in BIN_PACKAGES if include_bootstrap or name != BOOTSTRAP_BINARY]


def build_command(target, tool, include_bootstrap):
    command = [tool, "build", "--locked", "--release", "--target", target,
               "--no-default-features", "--features",
               "duduclaw-gateway/dashboard,duduclaw-cli/dashboard,"
               "duduclaw-gateway/vendored-openssl,duduclaw-parse-worker/engine,"
               "duduclaw-api-worker/native-tls"]
    for package in dict.fromkeys([*BIN_PACKAGES.values(), "duduclaw-gateway"]):
        command.extend(["-p", package])
    for name in archive_binaries(include_bootstrap):
        command.extend(["--bin", name])
    return command


def refuse_test_signer_build(command):
    """Test-key leak layer 1: no release build may compile the TEST-ONLY signer.

    The feature name, the binary name, and the selectors that would pull it in
    (--all-features, --bins, --all-targets) are all refused.
    """
    for token in command:
        if (TEST_SIGNER_FEATURE in token or TEST_SIGNER_BINARY in token
                or token in ("--all-features", "--bins", "--all-targets")):
            raise ValueError(f"build arguments enable the TEST-ONLY signer ({token}); refusing a release build")
    return command


def parse_bootstrap_pin(value):
    """Exactly 64 lowercase hex digits (sha256sum output), nothing else."""
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise ValueError("bootstrap root pin must be exactly 64 lowercase hex digits")
    return value


def parse_sha256(value, what):
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise ValueError(f"{what} must be exactly 64 lowercase hex digits")
    return value


def expected_bootstrap_pin(argument, environment, runs_build):
    """The explicit argument; the environment variable only feeds a build this run performs."""
    if argument is not None:
        return parse_bootstrap_pin(argument)
    if runs_build and environment.get(BOOTSTRAP_PIN_VARIABLE) is not None:
        return parse_bootstrap_pin(environment[BOOTSTRAP_PIN_VARIABLE])
    return None


def read_small(path, limit):
    """A regular file (never through a symlink) of at most `limit` bytes."""
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.EMLINK):
            raise ValueError(f"expected a regular file, not a symlink: {Path(path).name}") from error
        raise
    with os.fdopen(descriptor, "rb") as source:
        if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
            raise ValueError(f"expected a regular file: {Path(path).name}")
        raw = source.read(limit + 1)
    if len(raw) > limit:
        raise ValueError(f"{Path(path).name} exceeds {limit} bytes")
    return raw


def read_pin_file(path):
    """Production root pin from the public repository file; None when it is absent.

    Absent = no production pin = duduclaw-bootstrap is neither built nor shipped
    (fail closed). Present = exactly 64 lowercase hex digits and one newline; a
    symlink, a non-regular file or any other content is refused, never ignored.
    """
    try:
        raw = read_small(path, MAX_PIN_FILE_BYTES)
    except FileNotFoundError:
        return None
    if not raw.endswith(b"\n") or raw.count(b"\n") != 1:
        raise ValueError("production pin file must hold exactly one line: <64 lowercase hex>")
    try:
        text = raw[:-1].decode("ascii")
    except UnicodeDecodeError as error:
        raise ValueError("production pin file must be ASCII") from error
    return parse_bootstrap_pin(text)


def resolve_pin(argument, pin_file, environment, runs_build):
    """The one expected production pin of this run, or None (verifier not packaged)."""
    if pin_file is None:
        return expected_bootstrap_pin(argument, environment, runs_build)
    if argument is not None:
        raise ValueError("--bootstrap-root-sha256 and --bootstrap-root-pin-file are mutually exclusive")
    pin = read_pin_file(pin_file)
    inherited = environment.get(BOOTSTRAP_PIN_VARIABLE)
    if runs_build and inherited is not None and inherited != pin:
        raise ValueError(f"{BOOTSTRAP_PIN_VARIABLE} in the environment differs from the pin file; refusing")
    return pin


def build_environment(environment, pin, tool):
    """Cargo environment: the pin only when expected; cross must forward it into its container."""
    result = dict(environment)
    result.pop(BOOTSTRAP_PIN_VARIABLE, None)
    if pin is not None:
        result[BOOTSTRAP_PIN_VARIABLE] = pin
        if tool == "cross":
            names = result.get(CROSS_PASSTHROUGH_VARIABLE, "").split()
            if BOOTSTRAP_PIN_VARIABLE not in names:
                names.append(BOOTSTRAP_PIN_VARIABLE)
            result[CROSS_PASSTHROUGH_VARIABLE] = " ".join(names)
    return result


def rustflag_tokens(value, separator):
    if separator is not None:
        return [token for token in value.split(separator) if token]
    return value.split()


def enables_test_root(tokens):
    """True when the tokens configure `--cfg duduclaw_test_root` (any spelling rustc accepts)."""
    for index, token in enumerate(tokens):
        if token == TEST_ROOT_CFG:
            return True
        if token == "--cfg":
            value = tokens[index + 1] if index + 1 < len(tokens) else ""
        elif token.startswith("--cfg="):
            value = token.split("=", 1)[1]
        else:
            continue
        name = value.split("=", 1)[0].strip("'\"")
        if name == TEST_ROOT_CFG:
            return True
    return False


def refuse_test_root_environment(environment, target):
    """A pinned production verifier is never built or packaged next to test-root settings."""
    if BOOTSTRAP_TEST_KEY_VARIABLE in environment:
        raise ValueError(f"{BOOTSTRAP_TEST_KEY_VARIABLE} is set: refusing to package a production bootstrap verifier")
    target_variable = "CARGO_TARGET_" + re.sub(r"[^A-Za-z0-9]", "_", target).upper() + "_RUSTFLAGS"
    for variable, separator in (*RUSTFLAGS_VARIABLES, (target_variable, None)):
        value = environment.get(variable)
        if value is not None and enables_test_root(rustflag_tokens(value, separator)):
            raise ValueError(f"{variable} enables --cfg {TEST_ROOT_CFG}: refusing to package a production bootstrap verifier")


def host_architecture():
    """The release architecture this host can execute natively, or None."""
    if not sys.platform.startswith("linux"):
        return None
    return {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}.get(
        platform.machine().lower())


# ---------------------------------------------------------------- bootstrap verifier: one sealed flow
# _verified_bootstrap() is the ONLY code that reads, executes or archives the
# verifier, and it does all of it in one call: seal -> check seals -> hash ->
# check ELF target -> probe twice -> re-check -> (write member -> re-check) ->
# close. Nothing it produces (the plain BootstrapReport) is accepted back by any
# function, and no function takes a descriptor, digest or proof as evidence.
#
# Test seam (scripts/tests): where memfd does not exist (macOS) the unit tests
# replace _new_image, _add_seals and _get_seals TOGETHER with an unlinked-file
# stand-in. No option, argument or environment variable selects a stand-in, and
# off Linux every CLI path that would run the flow is refused first by
# require_native_target() (host_architecture() is None there).

BootstrapReport = collections.namedtuple("BootstrapReport", "target sha256 size pin version")


def _new_image():
    """An empty anonymous memfd that allows sealing, close-on-exec (Linux only)."""
    if not sys.platform.startswith("linux") or not hasattr(os, "memfd_create"):
        raise ValueError(f"{BOOTSTRAP_BINARY} can only be sealed for probing on Linux (memfd); "
                         "refusing to package it")
    flags = MFD_CLOEXEC | MFD_ALLOW_SEALING
    try:
        return os.memfd_create(BOOTSTRAP_BINARY, flags | MFD_EXEC)
    except OSError as error:
        if error.errno != errno.EINVAL:
            raise
    # Before Linux 6.3 MFD_EXEC is unknown and every memfd is executable.
    return os.memfd_create(BOOTSTRAP_BINARY, flags)


def _add_seals(descriptor):
    fcntl.fcntl(descriptor, F_ADD_SEALS, BOOTSTRAP_SEALS)


def _get_seals(descriptor):
    """The file's seals; 0 where it has none or the platform has no sealing (fail closed)."""
    if not sys.platform.startswith("linux"):
        return 0
    try:
        return fcntl.fcntl(descriptor, F_GET_SEALS)
    except OSError:
        return 0


def require_native_target(target):
    """The architecture of `target` when this host is Linux of it; refused otherwise.

    Called before anything is read or executed, so a verifier is only ever
    probed by a host that runs it natively, and for exactly the target whose
    archive the same call writes.
    """
    if target not in TARGETS:
        raise ValueError(f"unknown release target {target!r}")
    arch = TARGETS[target][0]
    if host_architecture() != arch:
        raise ValueError(f"{BOOTSTRAP_BINARY} for {arch} cannot be executed on this host, so its root pin "
                         f"cannot be verified; package {arch} on a native {arch} Linux host, or configure "
                         f"no production root pin to ship this architecture without {BOOTSTRAP_BINARY}")
    return arch


@contextlib.contextmanager
def _path_source(path):
    """(read, size) of a nonempty executable regular file, opened O_NOFOLLOW; closed on exit."""
    flags = os.O_RDONLY | os.O_NOFOLLOW | getattr(os, "O_CLOEXEC", 0) | os.O_NONBLOCK
    try:
        source = os.open(path, flags)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.EMLINK):
            raise ValueError(f"{BOOTSTRAP_BINARY} must not be a symbolic link") from error
        raise
    try:
        metadata = os.fstat(source)
        if (not stat.S_ISREG(metadata.st_mode) or not metadata.st_mode & 0o111
                or not 0 < metadata.st_size < MAX_BINARY_BYTES):
            raise ValueError(f"{BOOTSTRAP_BINARY} must be a nonempty executable regular file "
                             f"smaller than {MAX_BINARY_BYTES} bytes")
        yield (lambda size: os.read(source, size)), metadata.st_size
    finally:
        os.close(source)


@contextlib.contextmanager
def _member_source(bundle, member):
    """(read, size) of one regular member of an open release archive; never extracted to a path."""
    stream = bundle.extractfile(member) if member.isfile() else None
    if stream is None:
        raise ValueError(f"cannot read {member.name} from the release archive")
    try:
        yield stream.read, member.size
    finally:
        stream.close()


class _SealedImage:
    """The verifier's bytes in ONE anonymous sealed memfd, owned by one `with` block.

    __enter__ reads the source exactly once (it is closed again before anything
    is executed), seals the copy against write/shrink/grow/reseal and keeps the
    descriptor at >= 3 (pass_fds keeps its number in the probe child, whose 0-2
    are stdio). __exit__ closes it on every path; there is no finalizer.
    """

    def __init__(self, open_source):
        self._open_source = open_source
        self.descriptor = None

    def __enter__(self):
        image = _new_image()
        try:
            with self._open_source() as (read, expected_size):
                copied = 0
                while chunk := read(1024 * 1024):
                    copied += len(chunk)
                    if copied >= MAX_BINARY_BYTES:
                        raise ValueError(f"{BOOTSTRAP_BINARY} exceeds {MAX_BINARY_BYTES} bytes")
                    view = memoryview(chunk)
                    while view:
                        view = view[os.write(image, view):]
            if copied == 0 or copied != expected_size or os.fstat(image).st_size != copied:
                raise ValueError(f"{BOOTSTRAP_BINARY} changed size while it was read; refusing to package it")
            _add_seals(image)
            if image < 3:
                image, low = fcntl.fcntl(image, F_DUPFD_CLOEXEC, 3), image
                os.close(low)
        except BaseException:
            os.close(image)
            raise
        self.descriptor = image
        return self

    def __exit__(self, *exception):
        descriptor, self.descriptor = self.descriptor, None
        if descriptor is not None:
            os.close(descriptor)
        return False


def _fd_digest(descriptor):
    """(SHA-256 hex, size) of a descriptor's whole content, read by offset."""
    size = os.fstat(descriptor).st_size
    digest = hashlib.sha256()
    offset = 0
    while offset < size:
        chunk = os.pread(descriptor, min(1024 * 1024, size - offset), offset)
        if not chunk:
            raise ValueError(f"{BOOTSTRAP_BINARY} image ended early; refusing to package it")
        digest.update(chunk)
        offset += len(chunk)
    return digest.hexdigest(), size


def _require_sealed(descriptor):
    """Exactly the four seals, read back from the kernel (F_GET_SEALS), or refuse."""
    if _get_seals(descriptor) != BOOTSTRAP_SEALS:
        raise ValueError(f"{BOOTSTRAP_BINARY} image is not sealed against write/shrink/grow/reseal; "
                         "refusing to package it")


def _require_unchanged(descriptor, digest, size):
    _require_sealed(descriptor)
    if _fd_digest(descriptor) != (digest, size):
        raise ValueError(f"{BOOTSTRAP_BINARY} changed while it was being verified; refusing to package it")


def _require_target_elf(descriptor, arch):
    """A 64-bit little-endian ELF whose e_machine is the target's: with binfmt_misc
    emulation a host could otherwise execute (and accept) a foreign verifier."""
    header = os.pread(descriptor, 20, 0)
    if (len(header) < 20 or header[:4] != b"\x7fELF" or header[4] != 2 or header[5] != 1
            or int.from_bytes(header[18:20], "little") != ELF_MACHINES[arch]):
        raise ValueError(f"{BOOTSTRAP_BINARY} is not a 64-bit little-endian ELF for {arch}; refusing to package it")


class _PreadReader:
    """read() over a descriptor by offset: no second descriptor, no shared file position."""

    def __init__(self, descriptor, size):
        self._descriptor, self._size, self._offset = descriptor, size, 0

    def read(self, size=-1):
        remaining = self._size - self._offset
        if size is None or size < 0 or size > remaining:
            size = remaining
        chunk = os.pread(self._descriptor, size, self._offset) if size else b""
        self._offset += len(chunk)
        return chunk


def _probe_image(descriptor, flag):
    """Execute the sealed image (never a pathname) with one read-only flag; (exit code, stdout).

    The child execs /proc/self/fd/<descriptor>: pass_fds deliberately clears
    close-on-exec on that one descriptor in the child only (the parent's copy
    stays CLOEXEC). Empty environment, stdin /dev/null, stderr discarded, a fresh
    empty working directory, every other descriptor closed, a wall-clock bound
    and at most MAX_PROBE_OUTPUT_BYTES + 1 bytes of stdout read.
    """
    deadline = time.monotonic() + PROBE_TIMEOUT_SECONDS
    with tempfile.TemporaryDirectory(prefix="bootstrap-probe-") as workdir:
        process = subprocess.Popen([f"/proc/self/fd/{descriptor}", flag], env={}, cwd=workdir,
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   close_fds=True, pass_fds=(descriptor,))
        try:
            output = bytearray()
            stream = process.stdout.fileno()
            while len(output) <= MAX_PROBE_OUTPUT_BYTES:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise subprocess.TimeoutExpired(BOOTSTRAP_BINARY, PROBE_TIMEOUT_SECONDS)
                if select.select([stream], [], [], remaining)[0]:
                    chunk = os.read(stream, MAX_PROBE_OUTPUT_BYTES + 1 - len(output))
                    if not chunk:
                        break
                    output += chunk
            if len(output) > MAX_PROBE_OUTPUT_BYTES:
                return -1, bytes(output)  # refused by _check_probe; the child is killed below
            return process.wait(timeout=max(deadline - time.monotonic(), 0.001)), bytes(output)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdout.close()


def _check_probe(descriptor, flag, expected):
    try:
        code, output = _probe_image(descriptor, flag)
    except (OSError, subprocess.SubprocessError) as error:
        # Exec format error, missing loader, not executable, timeout: fail closed.
        raise ValueError(f"{BOOTSTRAP_BINARY} {flag} could not be executed on this host; "
                         "refusing to package it") from error
    if code != 0 or len(output) > MAX_PROBE_OUTPUT_BYTES or output != expected.encode("ascii"):
        # The output itself is not echoed: only the fixed expectation is named.
        raise ValueError(f"{BOOTSTRAP_BINARY} {flag} did not print exactly the expected value "
                         f"({expected.strip()}); refusing to package it")


def _verified_bootstrap(open_source, target, pin, version, *, bundle=None, expected_sha256=None):
    """THE sealed flow; returns a plain BootstrapReport after the descriptor is closed.

    `open_source` opens the verifier's only source ((read, size): a staged path
    or a member of an open release archive). In one call, which nothing can split:
    host check for `target` (before anything is read) -> read once into a memfd
    and seal it -> F_GET_SEALS must equal the four seals -> SHA-256 (and, when
    given, it must equal `expected_sha256`, which can only narrow what is
    accepted) -> ELF for the target -> `--print-root-pin` must print exactly
    `PRODUCTION <pin>` and `--version` this release's version -> seals and digest
    re-checked -> with `bundle`, the duduclaw-bootstrap member is written from the
    descriptor and seals and digest are checked once more -> close. A
    `cfg(duduclaw_test_root)` build prints `TEST <pin>` and is refused here
    (test-key leak layer 2). There is no attestation fallback.
    """
    arch = require_native_target(target)
    pin = parse_bootstrap_pin(pin)
    if not isinstance(version, str) or re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is None:
        raise ValueError(f"expected {BOOTSTRAP_BINARY} version must be x.y.z")
    if expected_sha256 is not None:
        parse_sha256(expected_sha256, f"expected {BOOTSTRAP_BINARY} SHA-256")
    with _SealedImage(open_source) as image:
        descriptor = image.descriptor
        _require_sealed(descriptor)
        digest, size = _fd_digest(descriptor)
        if expected_sha256 is not None and digest != expected_sha256:
            raise ValueError(f"{BOOTSTRAP_BINARY} differs from the handed-off {TRANSFER_SUMS}; refusing to package it")
        _require_target_elf(descriptor, arch)
        _check_probe(descriptor, "--print-root-pin", f"PRODUCTION {pin}\n")
        _check_probe(descriptor, "--version", f"{BOOTSTRAP_BINARY} {version}\n")
        # Before the member is written: still sealed, still the executed bytes.
        _require_unchanged(descriptor, digest, size)
        if bundle is not None:
            info = tarfile.TarInfo(BOOTSTRAP_BINARY)
            info.size = size
            info.mode = 0o755
            info.mtime = int(os.fstat(descriptor).st_mtime)
            info.uid, info.gid = os.getuid(), os.getgid()
            bundle.addfile(info, _PreadReader(descriptor, size))
            _require_unchanged(descriptor, digest, size)
    return BootstrapReport(target, digest, size, pin, version)


def check_bootstrap_file(path, target, pin, version):
    """Run the sealed flow on `path` without writing anything (--build-only check).

    Returns a plain BootstrapReport; no function accepts it back, so it cannot
    stand in for packaging, which runs the whole flow again.
    """
    return _verified_bootstrap(lambda: _path_source(path), target, pin, version)


def release_binary_dir(target):
    directory = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    if not directory.is_absolute():
        directory = ROOT / directory
    return directory / target / "release"


def validate_binary(path):
    metadata = path.lstat()
    if (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1
            or not metadata.st_mode & 0o111):
        raise ValueError(f"expected executable regular file with one link: {path.name}")
    if not 0 < metadata.st_size < MAX_BINARY_BYTES:
        raise ValueError(
            f"binary {path.name} is {metadata.st_size} bytes; must be nonempty and "
            f"smaller than {MAX_BINARY_BYTES // (1024 * 1024)} MiB ({MAX_BINARY_BYTES} bytes)")


def stage_cargo_binary(source, destination, require_executable=True):
    """Byte-copy a Cargo output into private staging, then validate the copy.

    Cargo hard-links target/<triple>/release/<bin> to release/deps/<bin>-<hash>,
    so the source legitimately has st_nlink == 2. The one-link rule is enforced
    on the private copy, which no other path can modify. Symlinks and non-regular
    files are still refused at the source (opened O_NOFOLLOW, no TOCTOU follow).
    A hand-off directory (--binary-dir) loses execute bits in transit, so there
    the caller passes require_executable=False and checks the copy's SHA-256.
    """
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(source, flags)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.EMLINK):
            raise ValueError(f"expected executable regular file with one link: {source.name}") from error
        raise
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or (require_executable and not metadata.st_mode & 0o111):
            raise ValueError(f"expected executable regular file with one link: {source.name}")
    except BaseException:
        os.close(descriptor)
        raise
    with os.fdopen(descriptor, "rb") as reader:
        with destination.open("xb") as writer:
            shutil.copyfileobj(reader, writer)
    destination.chmod(0o755)
    validate_binary(destination)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def private_copy(source, destination, limit):
    """Copy a regular file (never through a symlink) into private staging; SHA-256 of the copy."""
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(source, flags)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.EMLINK):
            raise ValueError(f"expected a regular file, not a symlink: {Path(source).name}") from error
        raise
    with os.fdopen(descriptor, "rb") as reader:
        metadata = os.fstat(reader.fileno())
        if not stat.S_ISREG(metadata.st_mode) or not 0 < metadata.st_size <= limit:
            raise ValueError(f"expected a nonempty regular file within the size limit: {Path(source).name}")
        with destination.open("xb") as writer:
            shutil.copyfileobj(reader, writer)
    destination.chmod(0o600)
    if not 0 < destination.stat().st_size <= limit:
        raise ValueError(f"{Path(source).name} changed while it was copied")
    return sha256(destination)


def create_archive(binary_dir, python_dir, target, output_dir, *, binaries, bootstrap_pin=None,
                   bootstrap_version=None, expected_bootstrap_sha256=None):
    """Write <output_dir>/duduclaw-linux-<arch>.tar.gz plus .sha256; returns (archive, report).

    `binaries` is archive_binaries(False): the verifier is never archived by name.
    Without `bootstrap_pin` the archive has no verifier and `report` is None.
    With it, binary_dir/duduclaw-bootstrap is the only source of the sealed flow
    (_verified_bootstrap), which this call runs while writing the archive: the
    member is written from the executed, sealed descriptor, for this `target`
    only (the archive is named for the same target). `bootstrap_version` and the
    optional `expected_bootstrap_sha256` (a hand-off SHA256SUMS entry) only
    narrow what is accepted; nothing a caller passes stands in for the execution.
    `report` is a plain BootstrapReport, produced after the descriptor is closed.
    Nothing is created under `output_dir` until the archive is complete.
    """
    if target not in TARGETS:
        raise ValueError(f"unknown release target {target!r}")
    if bootstrap_pin is None:
        if bootstrap_version is not None or expected_bootstrap_sha256 is not None:
            raise ValueError(f"{BOOTSTRAP_BINARY} expectations given without a production root pin")
    else:
        parse_bootstrap_pin(bootstrap_pin)
        require_native_target(target)  # refused before anything is read, executed or written
    if BOOTSTRAP_BINARY in binaries:
        raise ValueError(f"{BOOTSTRAP_BINARY} is never archived as a plain binary; "
                         "only the sealed flow (bootstrap_pin) adds it")
    if not set(archive_binaries(False)) <= set(binaries) <= set(BIN_PACKAGES):
        raise ValueError("incomplete or unknown release binary set")
    # Validate the whole set before creating any release output (the verifier's
    # path is only pre-checked here; the sealed flow reads it, once, below).
    for name in [*binaries, *([BOOTSTRAP_BINARY] if bootstrap_pin is not None else [])]:
        validate_binary(binary_dir / name)
    archive = output_dir / (TARGETS[target][1] + ".tar.gz")
    checksum = Path(str(archive) + ".sha256")
    if archive.exists() or checksum.exists():
        raise ValueError("release output already exists")
    if not python_dir.is_dir():
        raise ValueError("Python SDK directory is missing")
    report = None
    with tempfile.TemporaryDirectory(prefix="linux-release-") as temporary:
        stage = Path(temporary)
        for name in binaries:
            shutil.copyfile(binary_dir / name, stage / name)
            (stage / name).chmod(0o755)
            validate_binary(stage / name)
        for name, source in ARCHIVE_ALIASES.items():
            shutil.copyfile(stage / source, stage / name)
            (stage / name).chmod(0o755)
        staged_archive = stage / archive.name
        with tarfile.open(staged_archive, "w:gz", dereference=False) as bundle:
            for name in [*binaries, *ARCHIVE_ALIASES]:
                bundle.add(stage / name, arcname=name)
            if bootstrap_pin is not None:
                verifier = binary_dir / BOOTSTRAP_BINARY
                report = _verified_bootstrap(lambda: _path_source(verifier), target, bootstrap_pin,
                                             bootstrap_version, bundle=bundle,
                                             expected_sha256=expected_bootstrap_sha256)
            bundle.add(python_dir, arcname="python")
        # Re-read the written archive: the member must be the executed bytes.
        if report is not None:
            with tarfile.open(staged_archive, "r:gz") as bundle:
                members = [member for member in bundle.getmembers() if member.name == BOOTSTRAP_BINARY]
                content = bundle.extractfile(members[0]) if len(members) == 1 and members[0].isfile() else None
                actual = hashlib.sha256()
                while content is not None and (chunk := content.read(1024 * 1024)):
                    actual.update(chunk)
                if content is None or (actual.hexdigest(), members[0].size) != (report.sha256, report.size):
                    raise ValueError(f"archived {BOOTSTRAP_BINARY} differs from the executed bytes; "
                                     "refusing to package it")
        digest = sha256(staged_archive)
        output_dir.mkdir(parents=True, exist_ok=True)
        # Exclusive creation avoids silently replacing a previously signed file.
        with archive.open("xb") as destination, staged_archive.open("rb") as source:
            shutil.copyfileobj(source, destination)
        if sha256(archive) != digest:
            archive.unlink()
            raise ValueError(f"{archive.name} changed while it was written")
        with checksum.open("x", encoding="ascii") as destination:
            destination.write(f"{digest}  {archive.name}\n")
    return archive, report


# ---------------------------------------------------------------- native hand-off

def export_binaries(snapshot, names, destination, pin):
    """Write the frozen snapshot plus SHA256SUMS for a native packaging host.

    Returns the SHA-256 of SHA256SUMS: CI passes it to the packaging job through
    a job output (not the artifact store), and that job refuses any other value.
    """
    destination = Path(destination)
    destination.mkdir(mode=0o700)  # never reuses an existing directory
    lines = []
    for name in sorted(names):
        with (destination / name).open("xb") as writer, (snapshot / name).open("rb") as reader:
            shutil.copyfileobj(reader, writer)
        (destination / name).chmod(0o755)
        digest = sha256(destination / name)
        if digest != sha256(snapshot / name):
            raise ValueError(f"{name} changed while it was exported")
        lines.append(f"{digest}  {name}\n")
    with (destination / TRANSFER_SUMS).open("x", encoding="ascii") as output:
        output.write("".join(lines))
    sums = sha256(destination / TRANSFER_SUMS)
    bootstrap = (f"{BOOTSTRAP_BINARY} included UNPROBED (PRODUCTION {pin} expected by the native packager)"
                 if pin is not None else f"{BOOTSTRAP_BINARY} not built (no production root pin)")
    print(f"Exported {len(names)} binaries for native packaging; {TRANSFER_SUMS} sha256 {sums}; {bootstrap}.")
    return sums


def read_transfer_sums(directory, expected_digest, names):
    """Authenticate a hand-off directory: SHA256SUMS must hash to `expected_digest`
    and list exactly `names` (the set this run packages); nothing else may be present."""
    directory = Path(directory)
    expected_digest = parse_sha256(expected_digest, "--binary-sums-sha256")
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("--binary-dir must be a directory")
    raw = read_small(directory / TRANSFER_SUMS, MAX_TRANSFER_SUMS_BYTES)
    if hashlib.sha256(raw).hexdigest() != expected_digest:
        raise ValueError(f"{TRANSFER_SUMS} does not match --binary-sums-sha256; refusing the hand-off")
    try:
        text = raw.decode("ascii")
    except UnicodeDecodeError as error:
        raise ValueError(f"{TRANSFER_SUMS} is not ASCII") from error
    if not text.endswith("\n"):
        raise ValueError(f"{TRANSFER_SUMS} must end with a newline")
    entries = {}
    for line in text[:-1].split("\n"):
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]{1,128})", line)
        if match is None or match[2] in entries:
            raise ValueError(f"malformed or duplicate {TRANSFER_SUMS} line")
        entries[match[2]] = match[1]
    if set(entries) != set(names):
        raise ValueError("handed-off binary set differs from the set this run packages "
                         "(was the production pin configured differently on the build host?)")
    if {path.name for path in directory.iterdir()} != {*names, TRANSFER_SUMS}:
        raise ValueError("--binary-dir holds files other than the handed-off binaries")
    return entries


# ---------------------------------------------------------------- released archives

def verified_release_archive(archive, target, directory):
    """Private copy of a release tarball whose `.sha256` sidecar names exactly these bytes."""
    archive = Path(archive)
    if archive.name != TARGETS[target][1] + ".tar.gz":
        raise ValueError(f"{archive.name} is not the {TARGETS[target][0]} release archive")
    copy = Path(directory) / archive.name
    digest = private_copy(archive, copy, MAX_RELEASE_ARCHIVE_BYTES)
    sidecar = read_small(Path(str(archive) + ".sha256"), MAX_SIDECAR_BYTES)
    if sidecar != f"{digest}  {archive.name}\n".encode("ascii"):
        raise ValueError(f"{archive.name} does not match its .sha256 file")
    return copy, digest


def single_member(members, name, limit):
    matches = [member for member in members if member.name == name]
    if len(matches) != 1 or not matches[0].isfile() or not 0 < matches[0].size < limit:
        raise ValueError(f"release archive must contain exactly one regular file {name}")
    return matches[0]


def extract_member(bundle, member, destination):
    source = bundle.extractfile(member)
    if source is None:
        raise ValueError(f"cannot read {member.name} from the release archive")
    with destination.open("xb") as writer:
        shutil.copyfileobj(source, writer)
    destination.chmod(0o755)
    if destination.stat().st_size != member.size:
        raise ValueError(f"{member.name} has an unexpected size")


def verify_archive(archive, target, pin, version):
    """Check a finished release archive: with a pin its verifier is executed here
    and must report PRODUCTION <pin> and this version; without one it must be absent.

    The member is executed through the sealed flow from a sealed copy of exactly
    its archived bytes, never from an extracted path. Returns one summary line
    for the CI job summary."""
    arch = TARGETS[target][0]
    with tempfile.TemporaryDirectory(prefix="bootstrap-archive-check-") as temporary:
        stage = Path(temporary)
        stage.chmod(0o700)
        copy, digest = verified_release_archive(archive, target, stage)
        with tarfile.open(copy, "r:gz") as bundle:
            members = bundle.getmembers()
            shipped = [member for member in members if member.name == BOOTSTRAP_BINARY]
            if pin is None:
                if shipped:
                    raise ValueError(f"{Path(archive).name} contains {BOOTSTRAP_BINARY} although no production "
                                     "root pin is configured; refusing")
                return (f"{arch}: {BOOTSTRAP_BINARY} NOT shipped (no production root pin configured); "
                        f"archive sha256 {digest}")
            member = single_member(members, BOOTSTRAP_BINARY, MAX_BINARY_BYTES)
            report = _verified_bootstrap(lambda: _member_source(bundle, member), target, pin, version)
        return (f"{arch}: {BOOTSTRAP_BINARY} shipped, executed on this host: PRODUCTION {report.pin}, "
                f"sha256 {report.sha256}; archive sha256 {digest}")


def extract_bootstrap(archive, target, output_dir):
    """Copy the released verifier out as duduclaw-bootstrap-linux-<arch>. It is not
    executed here (scripts/release.sh probes it before anything is published);
    executing a verifier taken from an archive is verify_archive()'s sealed flow."""
    output_dir = Path(output_dir)
    destination = output_dir / (BOOTSTRAP_ASSET_PREFIX + TARGETS[target][0])
    if destination.exists():
        raise ValueError(f"{destination.name} already exists")
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="bootstrap-extract-") as temporary:
        stage = Path(temporary)
        copy, _ = verified_release_archive(archive, target, stage)
        with tarfile.open(copy, "r:gz") as bundle:
            extract_member(bundle, single_member(bundle.getmembers(), BOOTSTRAP_BINARY, MAX_BINARY_BYTES),
                           stage / BOOTSTRAP_BINARY)
        validate_binary(stage / BOOTSTRAP_BINARY)
        digest = sha256(stage / BOOTSTRAP_BINARY)
        with destination.open("xb") as writer, (stage / BOOTSTRAP_BINARY).open("rb") as reader:
            shutil.copyfileobj(reader, writer)
        destination.chmod(0o755)
        if sha256(destination) != digest:
            destination.unlink()
            raise ValueError(f"{destination.name} changed while it was written")
    print(f"{destination.name}: sha256 {digest} (extracted from {Path(archive).name}; not executed here)")
    return destination, digest


def snapshot_release_archive(archive, target, snapshot):
    """Freeze the production binaries of a released tarball into private staging."""
    copy, digest = verified_release_archive(archive, target, snapshot)
    names = archive_binaries(False)
    with tarfile.open(copy, "r:gz") as bundle:
        members = bundle.getmembers()
        for name in names:
            extract_member(bundle, single_member(members, name, MAX_BINARY_BYTES), snapshot / name)
            validate_binary(snapshot / name)
    copy.unlink()
    return names, digest


# ---------------------------------------------------------------- isolation release metadata

def release_serial(version):
    """Monotonic serial derived from the version: major*10^6 + minor*10^3 + patch."""
    if version["minor"] >= 1000 or version["patch"] >= 1000:
        raise ValueError("release serial needs minor and patch below 1000")
    return version["major"] * 1_000_000 + version["minor"] * 1_000 + version["patch"]


def render_release_template(raw, arch, version, now, valid_days):
    """Fill the per-release fields into the reviewed base template (release/isolation/)."""
    signer = load_signer()
    if len(raw) > signer.MAX_METADATA_BYTES:
        raise ValueError("release template exceeds metadata limit")
    document = json.loads(raw, object_pairs_hook=signer.unique_object,
                          parse_float=signer.reject_number, parse_constant=signer.reject_number)
    if not isinstance(document, dict):
        raise ValueError("release template must be an object")
    present = [field for field in PER_RELEASE_FIELDS if field in document]
    if present:
        raise ValueError("base release template must not set per-release fields: " + ", ".join(present))
    if not isinstance(valid_days, int) or isinstance(valid_days, bool) or not 1 <= valid_days <= MAX_VALID_DAYS:
        raise ValueError(f"manifest validity must be 1 to {MAX_VALID_DAYS} days")
    minimum = document.get("minimum_version")
    keys = ("major", "minor", "patch")
    if (not isinstance(minimum, dict) or set(minimum) != set(keys)
            or any(not isinstance(minimum[key], int) or isinstance(minimum[key], bool) for key in keys)
            or tuple(minimum[key] for key in keys) > tuple(version[key] for key in keys)):
        raise ValueError("minimum_version must be a version no newer than this release")
    rendered = dict(document, version=dict(version), arch=arch, serial=release_serial(version),
                    created_at_unix=now, expires_at_unix=now + valid_days * 86400)
    return signer.canonical_bytes(rendered)


def runtime_target(artifact):
    target = artifact.get("install_target", {})
    if target.get("kind") != "runtime_entry":
        raise ValueError("expected runtime entry")
    registry_id = target.get("registry_id")
    if (not isinstance(registry_id, str) or re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", registry_id) is None
            or registry_id in (".", "..") or set(target) != {"kind", "registry_id"}):
        raise ValueError("invalid runtime registry identifier")
    mode = 0o444 if registry_id == "isolation-policy-v1" else 0o755
    if artifact.get("mode") != mode:
        raise ValueError("invalid runtime artifact mode")
    return registry_id


def validate_targets(artifacts):
    if not isinstance(artifacts, list) or not len(REQUIRED_TARGETS) <= len(artifacts) <= 128:
        raise ValueError("incomplete bootstrap component set")
    identities = set()
    required = set()
    for artifact in artifacts:
        identifier = artifact.get("id")
        if isinstance(identifier, str) and (identifier == BOOTSTRAP_BINARY
                                            or identifier.startswith(BOOTSTRAP_BINARY + "-")):
            # The verifier ships only in the release tarball, written by the sealed
            # flow; it is never a signed artifact, an inbox file or a bundle member.
            raise ValueError(f"{identifier} names the root bootstrap verifier; it is never a signed artifact")
        kind = artifact.get("install_target", {}).get("kind")
        if kind == "runtime_entry":
            identity = (kind, runtime_target(artifact))
        elif kind in REQUIRED_TARGETS and artifact.get("mode") == 0o755:
            identity = (kind,)
            required.add(kind)
        else:
            raise ValueError("unknown release target or mode")
        if identity in identities:
            raise ValueError("duplicate release target")
        identities.add(identity)
    if required != REQUIRED_TARGETS:
        raise ValueError("incomplete bootstrap component set")


def runtime_source(artifact, binary_dir, runtime_dir):
    registry = runtime_target(artifact)
    builtin = {"api-worker-v1": "duduclaw-api-worker", "native-adapter-v1": "duduclaw-runtime-adapter"}
    if registry in builtin:
        return binary_dir / builtin[registry]
    if runtime_dir is None:
        raise ValueError("signed vendor/config artifacts require --runtime-artifacts-dir")
    return runtime_dir / ("runtime-" + registry)


def sign_release(binary_dir, target, output_dir, template, key, ssh_keygen, runtime_dir=None, test_signer=None,
                 target_root=None, target_root_sha256=None, ssh_sk_helper=None):
    """Sign the release manifest with the offline target key. With `target_root` (the pinned
    root.json, optionally checked against `target_root_sha256`) the signer refuses, writing
    nothing, unless the signature verifies against that root's target_key, so a wrong but valid
    key never produces outputs. `ssh_sk_helper` is the audited helper of `ssh_keygen` (FIDO2)."""
    if key is None:
        if template is not None:
            raise ValueError("--manifest-template requires an offline --signing-key")
        print("SKIP protected release manifest: offline target signing key not supplied; "
              "archive signing remains unchanged. Protected updates require separately signed metadata.")
        return []
    if os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS"):
        raise ValueError("offline isolation signing keys must never be used in CI")
    if template is None:
        raise ValueError("--signing-key requires a reviewed --manifest-template")
    signer = load_signer()
    raw = template.read_bytes()
    if len(raw) > signer.MAX_METADATA_BYTES:
        raise ValueError("release template exceeds metadata limit")
    document = json.loads(raw, object_pairs_hook=signer.unique_object,
                          parse_float=signer.reject_number, parse_constant=signer.reject_number)
    arch = TARGETS[target][0]
    if document.get("arch") != arch or document.get("os") != "linux":
        raise ValueError("release template architecture/OS does not match build target")
    expected_version = workspace_version()
    if (document.get("version") != expected_version or document.get("product") != "duduclaw"
            or document.get("schema_version") != 1 or document.get("channel") != "stable"):
        raise ValueError("release template must match this workspace version/product/schema/channel")
    artifacts = document.get("artifacts", [])
    validate_targets(artifacts)
    prefix = f"isolated-linux-{arch}-"
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="isolation-release-", dir=output_dir) as temporary:
        stage = Path(temporary)
        raw_artifacts = stage / "artifacts"
        raw_artifacts.mkdir()
        for artifact in artifacts:
            name = artifact.get("id")
            # Reuse the signer's identifier rule before using an ID as a path.
            if (not isinstance(name, str) or signer.re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", name) is None
                    or name in (".", "..")):
                raise ValueError("invalid release artifact ID/mode")
            kind = artifact["install_target"]["kind"]
            source = runtime_source(artifact, binary_dir, runtime_dir) if kind == "runtime_entry" else binary_dir / INSTALL_BINARIES[kind]
            if artifact["mode"] == 0o755:
                validate_binary(source)
            else:
                metadata = source.lstat()
                if (not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1
                        or not 0 < metadata.st_size <= 1024 * 1024):
                    raise ValueError("invalid signed runtime policy file")
                # Shape-check the policy (v2 domain/version, helper launcher
                # bindings) before its digest is bound; spawnd re-checks all of it.
                signer.validate_policy_document(json.loads(source.read_bytes(),
                    object_pairs_hook=signer.unique_object, parse_float=signer.reject_number,
                    parse_constant=signer.reject_number))
            # The signer rechecks immutable staged bytes, hashes and sizes. No
            # acceptance or policy is inferred from the local build outputs.
            shutil.copyfile(source, raw_artifacts / name)
        manifest_name = prefix + "manifest.json"
        names = [manifest_name, manifest_name + ".signatures.json"] + [prefix + a["id"] for a in artifacts]
        if len(set(names)) != len(names) or any((output_dir / name).exists() for name in names):
            raise ValueError("isolation release output already exists or collides")
        # Sign exactly the template bytes validated above, even if the external
        # template is edited while the operator touches the hardware key.
        staged_template = stage / "release-template.json"
        staged_template.write_bytes(raw)
        # Production: the key location reaches the signer only on its stdin (argv is
        # visible to other local processes; the environment is inherited by every
        # child); only the TEST signer takes --key. The signer gets the allow-listed
        # child environment, never this process's own.
        if test_signer is not None:
            signer_arguments = ["--key", str(key.resolve()), "--test-signer", str(Path(test_signer).resolve())]
            handoff = {"stdin": subprocess.DEVNULL}
        else:
            signer.refuse_key_variables()
            signer_arguments = ["--key-count", "1", "--ssh-keygen", ssh_keygen]
            if ssh_sk_helper is not None:
                signer_arguments += ["--ssh-sk-helper", str(ssh_sk_helper)]
            handoff = {"input": os.fsencode(str(key.resolve())) + b"\n"}
        if target_root is not None:
            signer_arguments += ["--root-metadata", str(Path(target_root).resolve())]
            if target_root_sha256 is not None:
                signer_arguments += ["--root-sha256", target_root_sha256]
        subprocess.run([sys.executable, "-I", "-B", str(ROOT / "scripts/sign-isolation-manifest.py"),
                        "--kind", "release", "--template", str(staged_template),
                        "--artifacts-dir", str(raw_artifacts), "--output", str(stage / manifest_name),
                        *signer_arguments], check=True, env=signer.child_environment(), **handoff)
        publication = [(manifest_name, stage / manifest_name),
                       (manifest_name + ".signatures.json", stage / (manifest_name + ".signatures.json"))]
        publication.extend((prefix + artifact["id"], raw_artifacts / artifact["id"]) for artifact in artifacts)
        for name, source_path in publication:
            with (output_dir / name).open("xb") as destination, source_path.open("rb") as source:
                shutil.copyfileobj(source, destination)
        return [output_dir / name for name in names]


def create_bootstrap_inbox(target, output_dir, root_path, root_signatures_path, expected_root_sha256=None):
    """Assemble only data for the compiled-pin verifier; never install or trust it.

    Inputs are externally signed metadata. The Rust --bootstrap-stage entrypoint
    checks the independent compiled pin, root threshold and target signatures.
    This helper deliberately cannot certify the legacy transition dual signature.
    Each input is read once; with `expected_root_sha256` (the pin) the root bytes
    read here must hash to it. scripts/release.sh passes only its private,
    already verified copies of root.json and its signatures.
    """
    prefix = f"isolated-linux-{TARGETS[target][0]}-"
    destination = output_dir / (prefix + "bootstrap-inbox")
    if destination.exists():
        raise ValueError("bootstrap inbox output already exists")
    signer = load_signer()
    def metadata(path, limit):
        with path.open("rb") as source:
            raw = source.read(limit + 1)
        if not raw or len(raw) > limit:
            raise ValueError("bootstrap metadata exceeds limit or is empty")
        value = json.loads(raw, object_pairs_hook=signer.unique_object,
                           parse_float=signer.reject_number, parse_constant=signer.reject_number)
        return raw, value
    root_bytes, root = metadata(root_path, signer.MAX_METADATA_BYTES)
    if expected_root_sha256 is not None and hashlib.sha256(root_bytes).hexdigest() != expected_root_sha256:
        raise ValueError("bootstrap root metadata does not match the expected root pin")
    root_signatures_bytes, root_signatures = metadata(root_signatures_path, 64 * 1024)
    if (signer.canonical_bytes(root) != root_bytes or not isinstance(root_signatures, list)
            or not 2 <= len(root_signatures) <= 6 or any(not isinstance(value, str) for value in root_signatures)):
        raise ValueError("expected canonical root metadata and threshold signatures")
    manifest_path = output_dir / (prefix + "manifest.json")
    manifest_bytes, manifest = metadata(manifest_path, signer.MAX_METADATA_BYTES)
    signature_bytes, signatures = metadata(Path(str(manifest_path) + ".signatures.json"), 16 * 1024)
    if (signer.canonical_bytes(manifest) != manifest_bytes or manifest.get("arch") != TARGETS[target][0]
            or not isinstance(signatures, list) or len(signatures) != 1 or not isinstance(signatures[0], str)):
        raise ValueError("expected canonical signed target manifest")
    artifacts = manifest.get("artifacts", [])
    validate_targets(artifacts)
    with tempfile.TemporaryDirectory(prefix="bootstrap-inbox-", dir=output_dir) as temporary:
        stage = Path(temporary) / "inbox"
        stage.mkdir(mode=0o700)
        (stage / "artifacts").mkdir(mode=0o700)
        for artifact in artifacts:
            name = artifact.get("id")
            if (not isinstance(name, str) or re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", name) is None
                    or name in (".", "..")):
                raise ValueError("invalid bootstrap artifact ID")
            source = output_dir / (prefix + name)
            copied = stage / "artifacts" / name
            with source.open("rb") as src, copied.open("xb") as dst:
                shutil.copyfileobj(src, dst)
            if copied.stat().st_size != artifact["size_bytes"] or list(bytes.fromhex(sha256(copied))) != artifact["sha256"]:
                raise ValueError("signed artifact changed before bootstrap assembly")
            copied.chmod(0o600)
        for name, payload in [("root.json", root_bytes), ("root.json.signatures.json", root_signatures_bytes),
                              ("manifest.json", manifest_bytes), ("manifest.json.signatures.json", signature_bytes)]:
            (stage / name).write_bytes(payload)
            (stage / name).chmod(0o600)
        stage.rename(destination)
    print(f"Bootstrap inbox data: {destination}; root fingerprint {hashlib.sha256(root_bytes).hexdigest()}. "
          "Not activated; compiled-pin verification and legacy transition authorization remain required.")
    return destination


# ---------------------------------------------------------------- migration bundle (plan §3.2)

def migration_bundle_name(arch, version):
    return f"duduclaw-isolated-linux-{arch}-{version_text(version)}.tar"


def write_ustar(path, members):
    """Plain POSIX ustar, regular files only, fixed owner/mode/mtime, in the given order."""
    with tarfile.open(path, "x", format=tarfile.USTAR_FORMAT) as bundle:
        for arcname, source in members:
            info = tarfile.TarInfo(arcname)
            info.size = source.stat().st_size
            info.mode = 0o600
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.type = tarfile.REGTYPE
            with source.open("rb") as reader:
                bundle.addfile(info, reader)


def _ustar_text(field):
    value, _, rest = field.partition(b"\0")
    if rest.strip(b"\0") or not value.isascii():
        raise ValueError("migration bundle header text is not NUL-terminated ASCII")
    return value.decode("ascii")


def _ustar_octal(field):
    text = field.rstrip(b"\0 ").lstrip(b" ")
    if not text or any(byte not in b"01234567" for byte in text):
        raise ValueError("migration bundle header number is not octal")
    return int(text, 8)


def check_ustar(path, expected):
    """Re-read the written bundle with the verifier's rules (duduclaw-updated bundle.rs
    read_ustar): ustar magic, regular files only, no link target, exact names in
    order, member bytes equal `expected` digests, zero padding and zero trailer."""
    with path.open("rb") as image:
        for arcname, digest in expected:
            header = image.read(512)
            if len(header) != 512 or header == bytes(512):
                raise ValueError("migration bundle is truncated")
            checksum = sum(header[:148]) + 8 * 32 + sum(header[156:])
            if (_ustar_octal(header[148:156]) != checksum or header[257:263] != b"ustar\0"
                    or header[263:265] != b"00" or header[156:157] != b"0" or any(header[157:257])):
                raise ValueError("migration bundle header is not a plain ustar regular file")
            name, prefix = _ustar_text(header[0:100]), _ustar_text(header[345:500])
            if (f"{prefix}/{name}" if prefix else name) != arcname:
                raise ValueError("migration bundle member order or name differs")
            size = _ustar_octal(header[124:136])
            actual = hashlib.sha256()
            remaining = size
            while remaining:
                chunk = image.read(min(remaining, 1024 * 1024))
                if not chunk:
                    raise ValueError("migration bundle is truncated")
                actual.update(chunk)
                remaining -= len(chunk)
            if actual.hexdigest() != digest:
                raise ValueError(f"migration bundle member {arcname} differs from the verified bytes")
            if any(image.read((512 - size % 512) % 512)):
                raise ValueError("migration bundle padding is not zero")
        trailer = image.read()
        if len(trailer) < 1024 or any(trailer):
            raise ValueError("migration bundle trailer is not two or more zero blocks")


def create_migration_bundle(target, inbox, output_dir):
    """duduclaw-isolated-linux-<arch>-<x.y.z>.tar from a bootstrap inbox (plan §3.2).

    Every member is copied into private staging and checked against the canonical
    signed manifest before it is written; the written image is re-read with the
    verifier's ustar rules. The tar is not authorized by this step: the root
    threshold BundleAuthorization and the legacy minisign signature follow.
    """
    arch = TARGETS[target][0]
    version = workspace_version()
    output_dir = Path(output_dir)
    destination = output_dir / migration_bundle_name(arch, version)
    if destination.exists():
        raise ValueError("migration bundle output already exists")
    inbox = Path(inbox)
    if inbox.is_symlink() or not inbox.is_dir():
        raise ValueError("bootstrap inbox must be a directory")
    if {path.name for path in inbox.iterdir()} != {*BUNDLE_METADATA, "artifacts"}:
        raise ValueError("bootstrap inbox must hold exactly the four metadata files and artifacts/")
    artifacts_dir = inbox / "artifacts"
    if artifacts_dir.is_symlink() or not artifacts_dir.is_dir():
        raise ValueError("bootstrap inbox artifacts/ must be a directory")
    signer = load_signer()
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="migration-bundle-", dir=output_dir) as temporary:
        stage = Path(temporary)
        limits = (signer.MAX_METADATA_BYTES, 64 * 1024, signer.MAX_METADATA_BYTES, 16 * 1024)
        documents = {}
        for entry, limit in zip(BUNDLE_METADATA, limits):
            private_copy(inbox / entry, stage / entry, limit)
            raw = (stage / entry).read_bytes()
            documents[entry] = (raw, json.loads(raw, object_pairs_hook=signer.unique_object,
                                                parse_float=signer.reject_number, parse_constant=signer.reject_number))
        (root_raw, root), (_, root_signatures) = documents["root.json"], documents["root.json.signatures.json"]
        (manifest_raw, manifest), (_, signatures) = documents["manifest.json"], documents["manifest.json.signatures.json"]
        if (signer.canonical_bytes(root) != root_raw or not isinstance(root_signatures, list)
                or not 2 <= len(root_signatures) <= 6 or any(not isinstance(value, str) for value in root_signatures)):
            raise ValueError("expected canonical root metadata and threshold signatures")
        if (signer.canonical_bytes(manifest) != manifest_raw or not isinstance(signatures, list)
                or len(signatures) != 1 or not isinstance(signatures[0], str)):
            raise ValueError("expected a canonical manifest with one target signature")
        if (manifest.get("arch") != arch or manifest.get("version") != version or manifest.get("os") != "linux"
                or manifest.get("product") != "duduclaw"):
            raise ValueError("manifest does not match this workspace version and target architecture")
        artifacts = manifest.get("artifacts", [])
        validate_targets(artifacts)
        identifiers = [artifact.get("id") for artifact in artifacts]
        if (any(not isinstance(name, str) or IDENTIFIER.fullmatch(name) is None or name in (".", "..")
                for name in identifiers) or len(set(identifiers)) != len(identifiers)):
            raise ValueError("invalid or duplicate artifact ID")
        if {path.name for path in artifacts_dir.iterdir()} != set(identifiers):
            raise ValueError("inbox artifacts differ from the signed manifest")
        (stage / "artifacts").mkdir(mode=0o700)
        members = [(entry, stage / entry) for entry in BUNDLE_METADATA]
        for artifact in artifacts:
            copied = stage / "artifacts" / artifact["id"]
            digest = private_copy(artifacts_dir / artifact["id"], copied, MAX_BINARY_BYTES)
            if copied.stat().st_size != artifact.get("size_bytes") or list(bytes.fromhex(digest)) != artifact.get("sha256"):
                raise ValueError(f"artifact {artifact['id']} differs from the signed manifest")
            members.append(("artifacts/" + artifact["id"], copied))
        staged = stage / destination.name
        write_ustar(staged, members)
        check_ustar(staged, [(arcname, sha256(path)) for arcname, path in members])
        size = staged.stat().st_size
        if not 0 < size <= MAX_BUNDLE_BYTES:
            raise ValueError("migration bundle exceeds 2 GiB")
        digest = sha256(staged)
        with destination.open("xb") as writer, staged.open("rb") as reader:
            shutil.copyfileobj(reader, writer)
        if sha256(destination) != digest:
            destination.unlink()
            raise ValueError("migration bundle changed while it was written")
    print(f"Migration bundle {destination.name}: sha256 {digest}, {size} bytes, root fingerprint "
          f"{hashlib.sha256(root_raw).hexdigest()}. NOT authorized yet: the root-threshold "
          "BundleAuthorization and the legacy minisign signature are separate offline steps.")
    return destination


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output-dir", type=Path, default=Path("dist-linux"))
    parser.add_argument("--build-tool", choices=("cargo", "cross"), default="cross")
    parser.add_argument("--build-only", action="store_true")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--manifest-template", type=Path)
    parser.add_argument("--runtime-artifacts-dir", type=Path,
                        help="reviewed runtime-<registry_id> ELF/policy files; no acceptance is generated")
    parser.add_argument("--signing-key", type=Path)
    parser.add_argument("--signing-key-stdin", action="store_true",
                        help="with --from-release-archive: read the offline target key location as one line "
                             "from stdin instead of --signing-key, so it is never in argv or the environment "
                             "(scripts/release.sh isolation-sign)")
    parser.add_argument("--ssh-keygen", default="ssh-keygen")
    parser.add_argument("--ssh-sk-helper", type=Path,
                        help="audited ssh-sk-helper of --ssh-keygen's installation, passed on to the signer "
                             "(scripts/release.sh isolation-sign resolves it)")
    parser.add_argument("--target-root", type=Path,
                        help="with --from-release-archive: the pinned root.json whose target_key must have made the "
                             "manifest signature (verified before anything is written); default: --bootstrap-root")
    parser.add_argument("--target-root-sha256",
                        help="expected SHA-256 (root pin) of --target-root and of --bootstrap-root")
    parser.add_argument("--bootstrap-root", type=Path, help="externally signed canonical root metadata for an inactive bootstrap inbox")
    parser.add_argument("--bootstrap-root-signatures", type=Path, help="root threshold signature JSON paired with --bootstrap-root")
    parser.add_argument("--bootstrap-root-sha256",
                        help="expected PRODUCTION root pin (64 lowercase hex) of duduclaw-bootstrap; the verifier "
                             "is then built, executed on this host (native architecture only) and packaged, or "
                             "the run fails. Omit it to package the archive without duduclaw-bootstrap")
    parser.add_argument("--bootstrap-root-pin-file", type=Path,
                        help="read the expected PRODUCTION pin from this public file (release/bootstrap-root.sha256); "
                             "an absent file means no pin: duduclaw-bootstrap is neither built nor packaged")
    parser.add_argument("--defer-bootstrap-probe", action="store_true",
                        help="--build-only on a host that cannot execute the target: build the pinned verifier "
                             "without executing it; packaging (always on a native host) still executes it")
    parser.add_argument("--export-binaries", type=Path,
                        help="--skip-build: write the frozen binaries plus SHA256SUMS to this NEW directory "
                             "for a native packaging host instead of packaging here")
    parser.add_argument("--binary-dir", type=Path,
                        help="--skip-build: package binaries handed off by --export-binaries")
    parser.add_argument("--binary-sums-sha256",
                        help="SHA-256 of the hand-off SHA256SUMS, received outside the hand-off itself")
    parser.add_argument("--verify-archive", type=Path,
                        help="check a finished release archive's verifier against the pin (or its absence)")
    parser.add_argument("--extract-bootstrap", type=Path,
                        help="copy the verifier of a released archive to <output-dir>/duduclaw-bootstrap-linux-<arch>")
    parser.add_argument("--from-release-archive", type=Path,
                        help="sign isolation assets over the binaries of a released archive (no new tarball)")
    parser.add_argument("--manifest-valid-days", type=int,
                        help="with --from-release-archive: render the base --manifest-template for this release "
                             "(version, arch, serial, created/expires)")
    parser.add_argument("--migration-bundle-from", type=Path,
                        help="write the plain-ustar migration bundle from a bootstrap inbox directory")
    parser.add_argument("--test-signer", type=Path,
                        help="TEST-ONLY (scripts/isolation-test-keys.sh): sign with duduclaw-isolation-test-signer; "
                             "only with --from-release-archive and never with a production pin")
    args = parser.parse_args()
    if args.signing_key_stdin:
        if args.signing_key is not None or args.test_signer is not None or args.from_release_archive is None:
            parser.error("--signing-key-stdin belongs to --from-release-archive and excludes --signing-key "
                         "and --test-signer")
        try:
            signer = load_signer()
            signer.refuse_key_variables()
            args.signing_key = signer.read_key_locations(1)[0]
        except ValueError as error:
            parser.error(str(error))
    try:
        check_release_binary_set()
    except ValueError as error:
        parser.error(str(error))
    standalone = [flag for flag, value in (("--verify-archive", args.verify_archive),
                                           ("--extract-bootstrap", args.extract_bootstrap),
                                           ("--from-release-archive", args.from_release_archive),
                                           ("--migration-bundle-from", args.migration_bundle_from)) if value is not None]
    if len(standalone) > 1:
        parser.error(" and ".join(standalone) + " are mutually exclusive")
    if standalone and (args.build_only or args.skip_build or args.export_binaries or args.binary_dir):
        parser.error(f"{standalone[0]} does not build or package; drop --build-only/--skip-build/--export-binaries/--binary-dir")
    if args.build_only and args.skip_build:
        parser.error("--build-only and --skip-build are mutually exclusive")
    if args.signing_key and (os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS")):
        parser.error("offline isolation signing keys must never be used in CI")
    if bool(args.bootstrap_root) != bool(args.bootstrap_root_signatures) or (args.bootstrap_root and not args.signing_key):
        parser.error("bootstrap inbox requires both root metadata files and the offline target signing key")
    if args.test_signer is not None and (args.from_release_archive is None or args.bootstrap_root_sha256
                                         or args.bootstrap_root_pin_file):
        parser.error("--test-signer is TEST-ONLY: accepted only with --from-release-archive and without a root pin")
    if args.manifest_valid_days is not None and args.from_release_archive is None:
        parser.error("--manifest-valid-days belongs to --from-release-archive")
    if (args.target_root or args.target_root_sha256) and args.from_release_archive is None:
        parser.error("--target-root/--target-root-sha256 belong to --from-release-archive")
    if args.target_root_sha256 is not None and re.fullmatch(r"[0-9a-f]{64}", args.target_root_sha256) is None:
        parser.error("--target-root-sha256 must be 64 lowercase hex digits")
    if args.ssh_sk_helper is not None and (args.test_signer is not None or not args.signing_key):
        parser.error("--ssh-sk-helper belongs to production signing with --ssh-keygen")
    if args.defer_bootstrap_probe and not args.build_only:
        parser.error("--defer-bootstrap-probe applies only to --build-only; packaging always executes the verifier")
    if (args.binary_dir is None) != (args.binary_sums_sha256 is None):
        parser.error("--binary-dir and --binary-sums-sha256 go together")
    if (args.binary_dir or args.export_binaries) and not args.skip_build:
        parser.error("--binary-dir/--export-binaries require --skip-build")
    if args.export_binaries and (args.binary_dir or args.manifest_template or args.signing_key):
        parser.error("--export-binaries only hands binaries off; it neither signs nor packages")
    version = version_text(workspace_version())

    if args.migration_bundle_from is not None:
        try:
            create_migration_bundle(args.target, args.migration_bundle_from, args.output_dir)
        except (OSError, ValueError) as error:
            parser.error(str(error))
        return
    if args.from_release_archive is not None:
        if args.bootstrap_root_sha256 or args.bootstrap_root_pin_file:
            parser.error("--from-release-archive never packages the verifier; probe it with --verify-archive "
                         "or scripts/release.sh isolation-sign")
        if args.signing_key is None or args.manifest_template is None:
            parser.error("--from-release-archive requires --signing-key and --manifest-template")
        if args.test_signer is not None:
            print("TEST-ONLY: isolation assets are signed with the test key set; they verify only "
                  "against a duduclaw_test_root build and must never be uploaded.")
        with tempfile.TemporaryDirectory(prefix="isolation-release-inputs-") as temporary:
            snapshot = Path(temporary)
            snapshot.chmod(0o700)
            names, digest = snapshot_release_archive(args.from_release_archive, args.target, snapshot)
            for name in names:
                subprocess.run([sys.executable, "-I", "-B", str(ROOT / "scripts/check_glibc.py"), str(snapshot / name)],
                               check=True, env=load_signer().child_environment(), stdin=subprocess.DEVNULL)
            template = args.manifest_template
            if args.manifest_valid_days is not None:
                template = snapshot / "rendered-release-template.json"
                template.write_bytes(render_release_template(args.manifest_template.read_bytes(),
                    TARGETS[args.target][0], workspace_version(), int(time.time()), args.manifest_valid_days))
            # The manifest signature must verify against the target_key of the pinned root (the
            # bootstrap inbox's root when there is one) before any output is written.
            signed = sign_release(snapshot, args.target, args.output_dir, template, args.signing_key,
                                  args.ssh_keygen, args.runtime_artifacts_dir, args.test_signer,
                                  target_root=args.target_root or args.bootstrap_root,
                                  target_root_sha256=args.target_root_sha256, ssh_sk_helper=args.ssh_sk_helper)
            if args.bootstrap_root:
                create_bootstrap_inbox(args.target, args.output_dir, args.bootstrap_root, args.bootstrap_root_signatures,
                                       expected_root_sha256=args.target_root_sha256)
        print(f"Signed {len(signed)} isolation assets over {args.from_release_archive.name} (sha256 {digest}).")
        return

    try:
        pin = resolve_pin(args.bootstrap_root_sha256, args.bootstrap_root_pin_file, os.environ,
                          not args.skip_build and args.verify_archive is None and args.extract_bootstrap is None)
    except ValueError as error:
        parser.error(str(error))
    if args.extract_bootstrap is not None:
        extract_bootstrap(args.extract_bootstrap, args.target, args.output_dir)
        return
    if args.verify_archive is not None:
        print(verify_archive(args.verify_archive, args.target, pin, version))
        return
    if pin is None:
        print(OMITTED_BOOTSTRAP_LINE)
    else:
        refuse_test_root_environment(os.environ, args.target)
    if not args.skip_build:
        command = refuse_test_signer_build(build_command(args.target, args.build_tool, pin is not None))
        subprocess.run(command, cwd=ROOT, check=True, env=build_environment(os.environ, pin, args.build_tool))
    if args.build_only:
        if pin is not None and args.defer_bootstrap_probe:
            print(DEFERRED_BOOTSTRAP_LINE)
        elif pin is not None:
            # Check the fresh build at once (sealed and executed, nothing written);
            # packaging runs the whole sealed flow again on its own snapshot.
            check_bootstrap_file(release_binary_dir(args.target) / BOOTSTRAP_BINARY, args.target, pin, version)
        # --build-only never packages, so a deferred verifier never reaches an archive.
        return
    binaries = archive_binaries(pin is not None)
    transfer = None
    if args.binary_dir is not None:
        try:
            transfer = read_transfer_sums(args.binary_dir, args.binary_sums_sha256, binaries)
        except (OSError, ValueError) as error:
            parser.error(str(error))
        binary_dir = args.binary_dir
    else:
        binary_dir = release_binary_dir(args.target)
    # Freeze a single input set for both the tarball and separately signed raw
    # assets; a concurrent build must not make these two outputs disagree.
    with tempfile.TemporaryDirectory(prefix="linux-release-inputs-") as temporary:
        snapshot = Path(temporary)
        snapshot.chmod(0o700)  # mkdtemp already creates 0700; keep it explicit
        for name in binaries:
            stage_cargo_binary(binary_dir / name, snapshot / name, require_executable=transfer is None)
            if transfer is not None and sha256(snapshot / name) != transfer[name]:
                raise ValueError(f"handed-off {name} does not match {TRANSFER_SUMS}; refusing to package it")
            subprocess.run([sys.executable, "-I", "-B", str(ROOT / "scripts/check_glibc.py"),
                            str(snapshot / name)], check=True)
        if args.export_binaries is not None:
            export_binaries(snapshot, binaries, args.export_binaries, pin)
            return
        # The archive first: create_archive runs the whole sealed flow for the
        # verifier (also with --skip-build and --binary-dir, where the sealed bytes
        # must also equal the handed-off SHA256SUMS entry) before any offline key
        # is touched. The snapshot path is only the flow's source.
        archive, report = create_archive(
            snapshot, ROOT / "python", args.target, args.output_dir, binaries=archive_binaries(False),
            bootstrap_pin=pin, bootstrap_version=version if pin is not None else None,
            expected_bootstrap_sha256=transfer[BOOTSTRAP_BINARY] if transfer is not None and pin is not None else None)
        signed = sign_release(snapshot, args.target, args.output_dir,
                              args.manifest_template, args.signing_key, args.ssh_keygen, args.runtime_artifacts_dir)
        if args.bootstrap_root:
            create_bootstrap_inbox(args.target, args.output_dir, args.bootstrap_root, args.bootstrap_root_signatures)
    if report is None:
        bootstrap = f"{BOOTSTRAP_BINARY} NOT included (no production root pin)"
    else:
        bootstrap = (f"{BOOTSTRAP_BINARY} included (executed here: PRODUCTION {report.pin}, "
                     f"sha256 {report.sha256})")
    print(f"Packaged {archive}; {bootstrap}; {len(signed)} protected update assets generated.")


if __name__ == "__main__":
    main()
