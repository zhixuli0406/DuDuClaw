#!/usr/bin/env python3
"""Refuse a TEST-licence gateway build on every path that signs, packages or publishes one.

A `--cfg duduclaw_test_root` duduclaw (or duduclaw-pro) also trusts a compiled TEST
licence issuer and embeds the constant TEST_LICENSE_BUILD_MARKER of
crates/duduclaw-gateway/src/license_runtime.rs. This script reads the marker from
that file at run time (exactly one definition, or it refuses to run, except that
source predating the test-licence issuer needs no scan), so the Rust constant stays
its only copy. A gateway binary is also refused when it contains one of
TEST_ONLY_COMMAND_MARKERS (strings of the TEST-only `ops users mfa-setup-code-test`
sub-command, compiled only into a test-root build). The check is static: nothing
is executed or extracted.

Scope
-----
This check stops a test-licence build from being shipped by mistake or through
stray or unknown configuration on the release machine. It is not a defence
against someone who controls that machine or toolchain: they could alter the
compiler, this script or the sources. The signed-source and review process
covers that threat.

  python3 -I -B scripts/refuse-test-license-build.py [--marker-file F] PATH...
      A PATH ending in .tar.gz, .tgz, .tar or .zip is an archive; anything else is a
      raw binary and is scanned whole. In an archive every regular member whose base
      name is duduclaw, duduclaw.exe, duduclaw-pro or duduclaw-pro.exe, or starts with
      `duduclaw-` and has no extension other than .exe, is read through the archive
      API (never extracted; absolute or `..` names are just names). A gateway-named
      link or special member, an unreadable member or a damaged archive is an error.
      --marker-file F takes the marker from F (its bytes, one optional trailing
      newline) where the source tree is not present.

  python3 -I -B scripts/refuse-test-license-build.py --check-environment
          [--target TRIPLE] [--cargo-dir DIR]...
      Refuses test-root build settings (shared with scripts/package-linux-release.py):
      DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY or DUDUCLAW_TEST_LICENSE_ISSUER_PUBKEY set;
      `--cfg duduclaw_test_root` in RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS,
      CARGO_BUILD_RUSTFLAGS or CARGO_TARGET_<TRIPLE>_RUSTFLAGS (only that target's with
      --target, every target's without), the corresponding RUSTDOCFLAGS forms or
      CARGO_PROFILE_*_RUSTFLAGS; response file tokens (@path) in those variables or
      Cargo configuration compiler flags (refused, never followed);
      RUSTC_WRAPPER, RUSTC_WORKSPACE_WRAPPER, RUSTC
      or their CARGO_BUILD_* forms set to anything (release builds use the toolchain's
      own rustc; no release job sets a wrapper); a .cargo/config.toml or .cargo/config
      containing `duduclaw_test_root` or a build.rustc/build.rustc-wrapper/
      build.rustc-workspace-wrapper setting in a --cargo-dir (default: the repository root
      and the current directory), any parent of it, or $CARGO_HOME (default ~/.cargo).

Exit status: 0 no marker / clean environment; 1 refused (one line per finding on
stderr, naming the file and member or the setting); 2 usage or I/O error. Callers
must treat every non-zero status as failure.
"""
import argparse
import os
from pathlib import Path
import re
import stat
import sys
import tarfile
import zipfile
import zlib

try:
    import tomllib
except ImportError:  # Python < 3.11: accept only unambiguous, simple TOML lines.
    tomllib = None

PROG = "refuse-test-license-build"
ROOT = Path(__file__).resolve().parents[1]
LICENSE_RUNTIME = ROOT / "crates/duduclaw-gateway/src/license_runtime.rs"
TEST_ROOT_CFG = "duduclaw_test_root"
TEST_LICENSE_ISSUER_VARIABLE = "DUDUCLAW_TEST_LICENSE_ISSUER_PUBKEY"
BOOTSTRAP_TEST_KEY_VARIABLE = "DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY"
# Flag variables Cargo hands to rustc, with their separators (Cargo reference,
# "Environment variables"); CARGO_TARGET_<TRIPLE>_RUSTFLAGS is checked as well.
RUSTFLAGS_VARIABLES = (("CARGO_ENCODED_RUSTFLAGS", "\x1f"), ("RUSTFLAGS", None), ("CARGO_BUILD_RUSTFLAGS", None),
                       ("CARGO_ENCODED_RUSTDOCFLAGS", "\x1f"), ("RUSTDOCFLAGS", None),
                       ("CARGO_BUILD_RUSTDOCFLAGS", None))
TARGET_RUSTFLAGS = re.compile(r"CARGO_TARGET_[A-Za-z0-9_]+_RUSTFLAGS")
PROFILE_RUSTFLAGS = re.compile(r"CARGO_PROFILE_[A-Za-z0-9_]+_RUSTFLAGS")
# Anything that replaces or wraps rustc could add the cfg out of sight.
RUSTC_VARIABLES = ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTC",
                   "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC")
RELEASE_ENV_VARIABLES = (BOOTSTRAP_TEST_KEY_VARIABLE, TEST_LICENSE_ISSUER_VARIABLE,
                         *(name for name, _ in RUSTFLAGS_VARIABLES), *RUSTC_VARIABLES)
TARGET_RUSTC = re.compile(r"CARGO_TARGET_[A-Za-z0-9_]+_RUSTC.*")
CARGO_CONFIG_NAMES = ("config.toml", "config")
RUSTC_CONFIG_KEYS = ("rustc-wrapper", "rustc-workspace-wrapper", "rustc")
# The one definition in license_runtime.rs (also parsed by container/Dockerfile.server).
MARKER_DEFINITION = re.compile(rb'^static TEST_LICENSE_BUILD_MARKER: &\[u8\] = b"([A-Za-z0-9_-]+)";\r?$', re.MULTILINE)
MARKER_DECLARATION = re.compile(rb"\bTEST_LICENSE_BUILD_MARKER\s*:")
MARKER_TEXT = re.compile(rb"[A-Za-z0-9_-]{16,}")
MIN_MARKER_BYTES = 16
MAX_MARKER_FILE_BYTES = 256
# E10 (2026-11-20): strings only a test-root duduclaw contains besides the licence
# marker: the TEST-only setup-code sub-command and its fixed stderr line
# (crates/duduclaw-cli/src/users_mfa_test_build.rs SUBCOMMAND / NOTICE, which its
# test-root unit test checks against this list). Scanned in every gateway binary
# together with the licence marker.
TEST_ONLY_COMMAND_MARKERS = (
    b"mfa-setup-code-test",
    b"TEST BUILD ONLY: issuing a first-run setup code for automated acceptance",
)
GATEWAY_NAMES = ("duduclaw", "duduclaw.exe", "duduclaw-pro", "duduclaw-pro.exe")
CHUNK_BYTES = 1024 * 1024


class RefuserError(ValueError):
    """The check could not be performed (usage or I/O problem; exit 2)."""


# ---------------------------------------------------------------- marker

def parse_marker(source):
    """The marker bytes of the single TEST_LICENSE_BUILD_MARKER definition in `source`."""
    if len(MARKER_DECLARATION.findall(source)) != 1:
        raise RefuserError("license_runtime.rs must declare TEST_LICENSE_BUILD_MARKER exactly once")
    definitions = MARKER_DEFINITION.findall(source)
    if len(definitions) != 1 or len(definitions[0]) < MIN_MARKER_BYTES:
        raise RefuserError("cannot read the one TEST_LICENSE_BUILD_MARKER definition in license_runtime.rs "
                           '(expected `static TEST_LICENSE_BUILD_MARKER: &[u8] = b"<marker>";`)')
    return definitions[0]


def load_marker(path=None):
    """Read the source marker, or return None for source predating the test issuer."""
    path = LICENSE_RUNTIME if path is None else path
    try:
        source = Path(path).read_bytes()
    except OSError as error:
        raise RefuserError(f"cannot read {path}: {error.strerror or error}") from error
    if not MARKER_DECLARATION.search(source) and TEST_ROOT_CFG.encode() not in source:
        return None
    return parse_marker(source)


def read_marker_file(path):
    try:
        with open(path, "rb") as source:
            raw = source.read(MAX_MARKER_FILE_BYTES + 1)
    except OSError as error:
        raise RefuserError(f"cannot read --marker-file {path}: {error.strerror or error}") from error
    if raw.endswith(b"\n"):
        raw = raw[:-1]
    if MARKER_TEXT.fullmatch(raw) is None:
        raise RefuserError("--marker-file must hold the marker alone: at least 16 of [A-Za-z0-9_-], "
                           "one optional trailing newline")
    return raw


def contains_marker(read, marker):
    """True when the bytes `read(size)` yields contain `marker` (also across chunk borders)."""
    return contains_any_marker(read, (marker,))


def contains_any_marker(read, markers):
    """True when the bytes `read(size)` yields contain any of `markers` (one pass, also across chunk borders)."""
    keep = max(len(marker) for marker in markers) - 1
    tail = b""
    while True:
        chunk = read(CHUNK_BYTES)
        if not chunk:
            return False
        window = tail + chunk
        if any(marker in window for marker in markers):
            return True
        tail = window[-keep:] if keep else b""


def release_markers(marker):
    """The licence marker plus the TEST-only command markers: none may occur in a release binary."""
    return (marker, *TEST_ONLY_COMMAND_MARKERS)


# ---------------------------------------------------------------- binaries and archives

def is_gateway_name(name):
    base = name.replace("\\", "/").rstrip("/").rsplit("/", 1)[-1].lower()
    if base in GATEWAY_NAMES:
        return True
    if not base.startswith("duduclaw-"):
        return False
    stem = base[:-4] if base.endswith(".exe") else base
    return "." not in stem


def archive_mode(path):
    name = Path(path).name.lower()
    if name.endswith((".tar.gz", ".tgz")):
        return "r|gz"
    if name.endswith(".tar"):
        return "r|"
    if name.endswith(".zip"):
        return "zip"
    return None


def _scan_raw(path, marker):
    with open(path, "rb") as source:
        if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
            raise RefuserError(f"{path} is not a regular file")
        return ([""] if contains_any_marker(source.read, release_markers(marker)) else []), 1


def _scan_tar(path, mode, marker):
    found, scanned = [], 0
    with tarfile.open(str(path), mode) as bundle:
        for member in bundle:
            if member.isdir() or not is_gateway_name(member.name):
                continue
            if not member.isreg():
                raise RefuserError(f"{path}:{member.name} is a link or special member, not a regular binary")
            source = bundle.extractfile(member)
            if source is None:
                raise RefuserError(f"cannot read {path}:{member.name}")
            scanned += 1
            if contains_any_marker(source.read, release_markers(marker)):
                found.append(member.name)
    return found, scanned


def _scan_zip(path, marker):
    found, scanned = [], 0
    with zipfile.ZipFile(str(path)) as bundle:
        for info in bundle.infolist():
            if info.is_dir():
                continue
            gateway = is_gateway_name(info.filename)
            kind = stat.S_IFMT(info.external_attr >> 16)
            if gateway and kind not in (0, stat.S_IFREG):
                raise RefuserError(f"{path}:{info.filename} is a link or special member, not a regular binary")
            # Every member is read (CRC-checked); only gateway members are searched.
            with bundle.open(info) as source:
                if gateway:
                    scanned += 1
                    if contains_any_marker(source.read, release_markers(marker)):
                        found.append(info.filename)
                else:
                    while source.read(CHUNK_BYTES):
                        pass
    return found, scanned


def scan(path, marker):
    """(labels that carry the marker or a TEST-only command marker, number of binaries scanned)
    for one raw binary or archive.

    A label is "" for a raw binary and the member name for an archive member."""
    mode = archive_mode(path)
    try:
        if mode is None:
            return _scan_raw(path, marker)
        if mode == "zip":
            return _scan_zip(path, marker)
        return _scan_tar(path, mode, marker)
    except RefuserError:
        raise
    except (OSError, EOFError, tarfile.TarError, zipfile.BadZipFile, zlib.error, RuntimeError,
            NotImplementedError, ValueError) as error:
        raise RefuserError(f"cannot read {path}: {error}") from error


# ---------------------------------------------------------------- build environment

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


def cargo_config_files(environment, cargo_dirs=None, cargo_home=None):
    """Cargo configuration files that apply to builds started in `cargo_dirs`."""
    directories = []
    for start in cargo_dirs or (ROOT,):
        current = Path(start).resolve()
        for directory in (current, *current.parents):
            if directory / ".cargo" not in directories:
                directories.append(directory / ".cargo")
    if cargo_home is None:
        cargo_home = environment.get("CARGO_HOME")
    if not cargo_home:
        try:
            cargo_home = Path.home() / ".cargo"
        except (KeyError, RuntimeError) as error:
            raise RefuserError("cannot determine CARGO_HOME (no home directory)") from error
    if Path(cargo_home) not in directories:
        directories.append(Path(cargo_home))
    return [directory / name for directory in directories for name in CARGO_CONFIG_NAMES]


def release_env_name(name):
    return (name in RELEASE_ENV_VARIABLES or TARGET_RUSTFLAGS.fullmatch(name)
            or PROFILE_RUSTFLAGS.fullmatch(name) or TARGET_RUSTC.fullmatch(name))


def has_response_file(tokens):
    """Refuse indirect compiler arguments without ever opening their files."""
    return any(token.startswith("@") for token in tokens)


def config_response_files(value, prefix=()):
    """Yield flag settings using response files anywhere in the decoded TOML tree."""
    if isinstance(value, dict):
        for key, item in value.items():
            path = (*prefix, key)
            if key in ("rustflags", "rustdocflags"):
                tokens = item.split() if isinstance(item, str) else item if isinstance(item, list) else []
                if has_response_file(token for token in tokens if isinstance(token, str)):
                    yield ".".join(path)
            yield from config_response_files(item, path)
    elif isinstance(value, list):
        for item in value:
            yield from config_response_files(item, prefix)


def decoded_strings(value):
    """Yield every decoded string, including table keys."""
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, item in value.items():
            yield from decoded_strings(key)
            yield from decoded_strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from decoded_strings(item)


def cargo_config_problems(data, path):
    """Return unsafe settings; an unreadable/ambiguous configuration fails closed."""
    problems = []
    if TEST_ROOT_CFG.encode("ascii") in data:
        problems.append(f"{path} mentions {TEST_ROOT_CFG}: refusing a production build or package")

    def refuse_key(key):
        if key[0] == "include":
            problems.append(f"{path}: include is not allowed in a release environment")
        elif len(key) >= 2 and key[0] == "build" and key[1] in RUSTC_CONFIG_KEYS:
            problems.append(f"{path} sets build.{key[1]}: release builds use the toolchain's own rustc; "
                            "refusing a production build or package")
        elif len(key) >= 2 and key[0] == "env" and release_env_name(key[1]):
            problems.append(f"{path} sets env.{key[1]}: not allowed in a release environment")

    try:
        text = data.decode("utf-8")
        if tomllib is not None:
            document = tomllib.loads(text)
            for key in config_response_files(document):
                problems.append(f"{path} sets {key} with a response file: not allowed in a release environment")
            if "include" in document:
                refuse_key(("include",))
            if (any(TEST_ROOT_CFG in value for value in decoded_strings(document))
                    and TEST_ROOT_CFG.encode("ascii") not in data):
                problems.append(f"{path} mentions decoded {TEST_ROOT_CFG}: refusing a production build or package")
            build = document.get("build", {})
            if not isinstance(build, dict):
                raise ValueError("build must be a table")
            for key in RUSTC_CONFIG_KEYS:
                if key in build:
                    refuse_key(("build", key))
            env = document.get("env", {})
            if not isinstance(env, dict):
                raise ValueError("env must be a table")
            for name, item in env.items():
                refuse_key(("env", name))
                value = item.get("value") if isinstance(item, dict) else item
                if "FLAGS" in name and isinstance(value, str) and has_response_file(value.split()):
                    problems.append(f"{path} sets env.{name} with a response file: not allowed in a release environment")
            return problems

        # This is deliberately a small subset, not a replacement TOML parser.
        # Quoted keys, escapes, inline tables and multiline values are ambiguous
        # here and require Python 3.11+ rather than risking a missed override.
        if re.search(r"\\[uU]", text) or '"""' in text or "'''" in text:
            raise ValueError("cannot safely decode Cargo configuration without tomllib (use Python 3.11+)")
        # Conservatively reject even unrelated @ characters with flag words here:
        # arrays can span lines, and this fallback cannot safely decode their scope.
        if "@" in text and re.search(r"\b(?:rustflags|rustdocflags)\b", text):
            problems.append(f"{path}: possible response file in compiler flags without tomllib; "
                            "not allowed in a release environment")
        key_pattern = r"[A-Za-z0-9_-]+(?:\s*\.\s*[A-Za-z0-9_-]+)*"
        string_pattern = r'''(?:"[^"\\\r\n]*"|'[^'\r\n]*')'''
        value_pattern = rf"(?:{string_pattern}|true|false|[+-]?(?:0|[1-9][0-9]*)|\[\s*(?:{string_pattern}\s*,\s*)*(?:{string_pattern}\s*)?\])"
        table, seen, tables = (), set(), set()
        for line in text.splitlines():
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            header = re.fullmatch(rf"\s*\[\s*({key_pattern})\s*\]\s*(?:#.*)?", line)
            if header:
                table = tuple(part.strip() for part in header[1].split("."))
                if table in tables or any(table[:len(key)] == key for key in seen):
                    raise ValueError("duplicate or conflicting table")
                tables.add(table)
                refuse_key(table)
                continue
            setting = re.fullmatch(rf"\s*({key_pattern})\s*=\s*{value_pattern}\s*(?:#.*)?", line)
            if setting is None:
                raise ValueError("cannot safely parse Cargo configuration without tomllib (use Python 3.11+)")
            key = table + tuple(part.strip() for part in setting[1].split("."))
            if (any(key[:len(other)] == other or other[:len(key)] == key for other in seen)
                    or any(other[:len(key)] == key for other in tables)):
                raise ValueError("duplicate or conflicting key")
            seen.add(key)
            refuse_key(key)
            if len(key) >= 2 and key[0] == "env" and "FLAGS" in key[1] and "@" in line:
                problems.append(f"{path} sets env.{key[1]} with a possible response file: "
                                "not allowed in a release environment")
        return problems
    except ValueError as error:
        raise RefuserError(f"cannot parse Cargo configuration {path}: {error}") from error


def environment_problems(environment, target=None, cargo_dirs=None, cargo_home=None):
    """Every test-root build setting in `environment` and the applicable Cargo configuration."""
    problems = []
    for variable in (*RELEASE_ENV_VARIABLES,
                     *sorted(name for name in environment if TARGET_RUSTC.fullmatch(name))):
        if variable in dict(RUSTFLAGS_VARIABLES):
            continue
        if variable in environment:
            reason = ("release builds use the toolchain's own rustc; "
                      if variable in RUSTC_VARIABLES or TARGET_RUSTC.fullmatch(variable) else "")
            problems.append(f"{variable} is set: {reason}refusing a production build or package")
    if target is None:
        target_variables = sorted(name for name in environment if TARGET_RUSTFLAGS.fullmatch(name))
    else:
        target_variables = ["CARGO_TARGET_" + re.sub(r"[^A-Za-z0-9]", "_", target).upper() + "_RUSTFLAGS"]
    profile_variables = sorted(name for name in environment if PROFILE_RUSTFLAGS.fullmatch(name))
    for variable, separator in (*RUSTFLAGS_VARIABLES,
                                *((name, None) for name in (*target_variables, *profile_variables))):
        value = environment.get(variable)
        tokens = rustflag_tokens(value, separator) if value is not None else []
        if enables_test_root(tokens):
            problems.append(f"{variable} enables --cfg {TEST_ROOT_CFG}: refusing a production build or package")
        if has_response_file(tokens):
            problems.append(f"{variable} contains a response file: refusing a production build or package")
    for path in cargo_config_files(environment, cargo_dirs, cargo_home):
        try:
            data = path.read_bytes()
        except FileNotFoundError:
            continue
        except OSError as error:
            raise RefuserError(f"cannot read Cargo configuration {path}: {error.strerror or error}") from error
        problems.extend(cargo_config_problems(data, path))
    return problems


def refuse_test_root_environment(environment, target=None, cargo_dirs=None, cargo_home=None):
    """Raise ValueError (the first problem) unless the build environment is free of test-root settings."""
    problems = environment_problems(environment, target, cargo_dirs, cargo_home)
    if problems:
        raise ValueError(problems[0])


# ---------------------------------------------------------------- command line

def _emit(text, stream=None):
    stream = sys.stderr if stream is None else stream
    encoding = getattr(stream, "encoding", None) or "ascii"
    stream.write(text.encode(encoding, "backslashreplace").decode(encoding) + "\n")
    stream.flush()


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="refuse-test-license-build.py",
        description="Exit 1 when a binary/archive is a TEST-licence gateway build or the build "
                    "environment carries test-root settings; exit 2 on usage or I/O errors.")
    parser.add_argument("paths", nargs="*", metavar="PATH", help="raw binary or .tar.gz/.tgz/.tar/.zip archive")
    parser.add_argument("--marker-file", help="read the marker from this file instead of license_runtime.rs")
    parser.add_argument("--check-environment", action="store_true",
                        help="refuse test-root build settings in the environment and Cargo configuration")
    parser.add_argument("--target", help="with --check-environment: check only this target's CARGO_TARGET_*_RUSTFLAGS")
    parser.add_argument("--cargo-dir", action="append", dest="cargo_dirs",
                        help="with --check-environment: a directory Cargo runs in (repeatable)")
    args = parser.parse_args(argv)
    if not args.paths and not args.check_environment:
        parser.error("name at least one binary or archive, or --check-environment")
    if (args.target is not None or args.cargo_dirs) and not args.check_environment:
        parser.error("--target and --cargo-dir belong to --check-environment")
    if args.marker_file is not None and not args.paths:
        parser.error("--marker-file needs a binary or archive to scan")
    refused = failed = False
    if args.check_environment:
        directories = args.cargo_dirs or [str(ROOT), os.getcwd()]
        try:
            problems = environment_problems(os.environ, args.target, directories)
        except RefuserError as error:
            _emit(f"{PROG}: ERROR {error}")
            return 2
        for problem in problems:
            _emit(f"{PROG}: REFUSED {problem}")
        if problems:
            refused = True
        else:
            _emit(f"{PROG}: OK build environment has no test-root setting", sys.stdout)
    if args.paths:
        try:
            marker = read_marker_file(args.marker_file) if args.marker_file is not None else load_marker()
        except RefuserError as error:
            _emit(f"{PROG}: ERROR {error}")
            return 2
        if marker is None:
            _emit(f"{PROG}: source predates the test-licence issuer; skipping marker scan", sys.stdout)
            return 1 if refused else 0
        for path in args.paths:
            try:
                found, scanned = scan(path, marker)
            except RefuserError as error:
                _emit(f"{PROG}: ERROR {error}")
                failed = True
                continue
            for member in found:
                label = f"{path}:{member}" if member else path
                _emit(f"{PROG}: REFUSED {label} is a TEST-licence gateway build (cfg({TEST_ROOT_CFG}) marker); "
                      "it must never be signed, packaged or published")
            if found:
                refused = True
            else:
                _emit(f"{PROG}: OK {path}: no TEST-licence marker ({scanned} gateway binaries scanned)", sys.stdout)
    if refused:
        return 1
    return 2 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
