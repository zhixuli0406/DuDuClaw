#!/usr/bin/env python3
"""Build/package the complete native Linux release; offline signing is optional.

The caller builds web/dist first. Without --signing-key this produces the normal
tarball and SHA-256 only. Offline operators may supply a reviewed F release
template and their external target key; no key is copied and CI cannot sign.

The root bootstrap verifier (duduclaw-bootstrap) is packaged only with an
explicit production root pin (--bootstrap-root-sha256). Without one it is left
out entirely. With one, a test-root build environment is refused and the staged
binary must report exactly `PRODUCTION <pin>` and this release's version.
"""
import argparse
import errno
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-unknown-linux-gnu": ("x86_64", "duduclaw-linux-x64"),
    "aarch64-unknown-linux-gnu": ("aarch64", "duduclaw-linux-arm64"),
}
# Select explicit production bins; --bins would also select root-test fixtures.
BIN_PACKAGES = {
    "duduclaw": "duduclaw-cli",
    "duduclaw-spawnd": "duduclaw-spawnd",
    "duduclaw-run": "duduclaw-run",
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
    "trampoline": "duduclaw-run", "egress": "duduclaw-egress",
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

# Root bootstrap verifier (plan R1 §4.1, R2 §4.2). Its root pin is compiled in
# through this variable; a `--cfg duduclaw_test_root` build reports TEST.
BOOTSTRAP_BINARY = "duduclaw-bootstrap"
BOOTSTRAP_PIN_VARIABLE = "DUDUCLAW_BOOTSTRAP_ROOT_SHA256"
BOOTSTRAP_TEST_KEY_VARIABLE = "DUDUCLAW_BOOTSTRAP_TEST_MINISIGN_KEY"
TEST_ROOT_CFG = "duduclaw_test_root"
# Flag variables Cargo hands to rustc, with their separators (Cargo reference,
# "Environment variables"); CARGO_TARGET_<TRIPLE>_RUSTFLAGS is checked as well.
RUSTFLAGS_VARIABLES = (("CARGO_ENCODED_RUSTFLAGS", "\x1f"), ("RUSTFLAGS", None), ("CARGO_BUILD_RUSTFLAGS", None))
PIN_MARKER_HEADER = "duduclaw-bootstrap-pin-marker v1"
MAX_PIN_MARKER_BYTES = 4096
MAX_PROBE_OUTPUT_BYTES = 256
OMITTED_BOOTSTRAP_LINE = ("SKIP duduclaw-bootstrap: no production root pin configured "
                          "(--bootstrap-root-sha256); the root bootstrap verifier is not packaged.")


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


def parse_bootstrap_pin(value):
    """Exactly 64 lowercase hex digits (sha256sum output), nothing else."""
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise ValueError("bootstrap root pin must be exactly 64 lowercase hex digits")
    return value


def expected_bootstrap_pin(argument, environment, runs_build):
    """The explicit argument; the environment variable only feeds a build this run performs."""
    if argument is not None:
        return parse_bootstrap_pin(argument)
    if runs_build and environment.get(BOOTSTRAP_PIN_VARIABLE) is not None:
        return parse_bootstrap_pin(environment[BOOTSTRAP_PIN_VARIABLE])
    return None


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


def probe_bootstrap(path, flag):
    """Run the staged verifier with one read-only flag; (exit code, stdout bytes)."""
    result = subprocess.run([str(path), flag], env={}, stdin=subprocess.DEVNULL,
                            capture_output=True, timeout=30, check=False)
    return result.returncode, result.stdout


def pin_marker_text(arch, version, digest, pin):
    return (f"{PIN_MARKER_HEADER}\narch {arch}\nversion {version}\nsha256 {digest}\n"
            f"print-root-pin PRODUCTION {pin}\n")


def read_pin_marker(path):
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ValueError("bootstrap pin marker is missing or not a regular file") from error
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_PIN_MARKER_BYTES:
            raise ValueError("bootstrap pin marker is missing or not a regular file")
        raw = source.read(MAX_PIN_MARKER_BYTES + 1)
    if len(raw) > MAX_PIN_MARKER_BYTES:
        raise ValueError("bootstrap pin marker exceeds its size limit")
    return raw


def check_probe(path, flag, expected):
    code, output = probe_bootstrap(path, flag)
    if code != 0 or len(output) > MAX_PROBE_OUTPUT_BYTES or output != expected.encode("ascii"):
        # The output itself is not echoed: only the fixed expectation is named.
        raise ValueError(f"{BOOTSTRAP_BINARY} {flag} did not print exactly the expected value "
                         f"({expected.strip()}); refusing to package it")


def verify_bootstrap_binary(path, target, pin, version, marker=None):
    """Require the exact verifier bytes to be a PRODUCTION build of this pin and version.

    A host-compatible binary is executed. A foreign-architecture binary cannot be,
    so a marker produced on that architecture (--emit-bootstrap-pin-marker) for
    exactly these bytes, pin and version is required instead.
    """
    arch = TARGETS[target][0]
    digest = sha256(path)
    if host_architecture() == arch:
        check_probe(path, "--print-root-pin", f"PRODUCTION {pin}\n")
        check_probe(path, "--version", f"{BOOTSTRAP_BINARY} {version}\n")
        if marker is None:
            return
    elif marker is None:
        raise ValueError(f"{BOOTSTRAP_BINARY} for {arch} cannot be executed on this host; pass "
                         "--bootstrap-pin-verified-by with a marker produced on that architecture")
    if read_pin_marker(marker) != pin_marker_text(arch, version, digest, pin).encode("ascii"):
        raise ValueError("bootstrap pin marker does not attest these exact bytes, architecture, version and pin")


def emit_pin_marker(binary, target, pin, version, output):
    """On a host of the target architecture: execute the verifier, then attest it."""
    arch = TARGETS[target][0]
    if host_architecture() != arch:
        raise ValueError(f"a {arch} pin marker must be produced on a {arch} Linux host")
    with tempfile.TemporaryDirectory(prefix="bootstrap-marker-") as temporary:
        copy = Path(temporary) / BOOTSTRAP_BINARY
        stage_cargo_binary(binary, copy)
        verify_bootstrap_binary(copy, target, pin, version)
        text = pin_marker_text(arch, version, sha256(copy), pin)
    with output.open("x", encoding="ascii") as destination:
        destination.write(text)
    return output


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


def stage_cargo_binary(source, destination):
    """Byte-copy a Cargo output into private staging, then validate the copy.

    Cargo hard-links target/<triple>/release/<bin> to release/deps/<bin>-<hash>,
    so the source legitimately has st_nlink == 2. The one-link rule is enforced
    on the private copy, which no other path can modify. Symlinks and non-regular
    files are still refused at the source (opened O_NOFOLLOW, no TOCTOU follow).
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
        if not stat.S_ISREG(metadata.st_mode) or not metadata.st_mode & 0o111:
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


def create_archive(binary_dir, python_dir, target, output_dir, *, binaries):
    """`binaries` comes from archive_binaries(); the verifier only after verify_bootstrap_binary."""
    if not set(archive_binaries(False)) <= set(binaries) <= set(BIN_PACKAGES):
        raise ValueError("incomplete or unknown release binary set")
    # Validate the whole set before creating any release output.
    for name in binaries:
        validate_binary(binary_dir / name)
    archive = output_dir / (TARGETS[target][1] + ".tar.gz")
    checksum = Path(str(archive) + ".sha256")
    if archive.exists() or checksum.exists():
        raise ValueError("release output already exists")
    if not python_dir.is_dir():
        raise ValueError("Python SDK directory is missing")
    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="linux-release-", dir=output_dir) as temporary:
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
            bundle.add(python_dir, arcname="python")
        digest = sha256(staged_archive)
        # Exclusive creation avoids silently replacing a previously signed file.
        with archive.open("xb") as destination, staged_archive.open("rb") as source:
            shutil.copyfileobj(source, destination)
        with checksum.open("x", encoding="ascii") as destination:
            destination.write(f"{digest}  {archive.name}\n")
    return archive


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


def sign_release(binary_dir, target, output_dir, template, key, ssh_keygen, runtime_dir=None):
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
    spec = importlib.util.spec_from_file_location("isolation_signer", ROOT / "scripts/sign-isolation-manifest.py")
    signer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(signer)
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
        subprocess.run([sys.executable, str(ROOT / "scripts/sign-isolation-manifest.py"),
                        "--kind", "release", "--template", str(staged_template),
                        "--artifacts-dir", str(raw_artifacts), "--output", str(stage / manifest_name),
                        "--key", str(key.resolve()), "--ssh-keygen", ssh_keygen], check=True)
        publication = [(manifest_name, stage / manifest_name),
                       (manifest_name + ".signatures.json", stage / (manifest_name + ".signatures.json"))]
        publication.extend((prefix + artifact["id"], raw_artifacts / artifact["id"]) for artifact in artifacts)
        for name, source_path in publication:
            with (output_dir / name).open("xb") as destination, source_path.open("rb") as source:
                shutil.copyfileobj(source, destination)
        return [output_dir / name for name in names]


def create_bootstrap_inbox(target, output_dir, root_path, root_signatures_path):
    """Assemble only data for the compiled-pin verifier; never install or trust it.

    Inputs are externally signed metadata. The Rust --bootstrap-stage entrypoint
    checks the independent compiled pin, root threshold and target signatures.
    This helper deliberately cannot certify the legacy transition dual signature.
    """
    prefix = f"isolated-linux-{TARGETS[target][0]}-"
    destination = output_dir / (prefix + "bootstrap-inbox")
    if destination.exists():
        raise ValueError("bootstrap inbox output already exists")
    spec = importlib.util.spec_from_file_location("isolation_signer", ROOT / "scripts/sign-isolation-manifest.py")
    signer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(signer)
    def metadata(path, limit):
        with path.open("rb") as source:
            raw = source.read(limit + 1)
        if not raw or len(raw) > limit:
            raise ValueError("bootstrap metadata exceeds limit or is empty")
        value = json.loads(raw, object_pairs_hook=signer.unique_object,
                           parse_float=signer.reject_number, parse_constant=signer.reject_number)
        return raw, value
    root_bytes, root = metadata(root_path, signer.MAX_METADATA_BYTES)
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--output-dir", type=Path, default=Path("dist-linux"))
    parser.add_argument("--build-tool", choices=("cargo", "cross"), default="cross")
    parser.add_argument("--build-only", action="store_true")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--manifest-template", type=Path)
    parser.add_argument("--runtime-artifacts-dir", type=Path,
                        help="reviewed runtime-<registry_id> ELF/policy files; no acceptance is generated")
    parser.add_argument("--signing-key", type=Path)
    parser.add_argument("--ssh-keygen", default="ssh-keygen")
    parser.add_argument("--bootstrap-root", type=Path, help="externally signed canonical root metadata for an inactive bootstrap inbox")
    parser.add_argument("--bootstrap-root-signatures", type=Path, help="root threshold signature JSON paired with --bootstrap-root")
    parser.add_argument("--bootstrap-root-sha256",
                        help="expected PRODUCTION root pin (64 lowercase hex) of duduclaw-bootstrap; without it "
                             "the verifier is not packaged")
    parser.add_argument("--bootstrap-pin-verified-by", type=Path,
                        help="pin marker produced on the target architecture (--emit-bootstrap-pin-marker) when "
                             "the target's duduclaw-bootstrap cannot be executed on this host")
    parser.add_argument("--emit-bootstrap-pin-marker", type=Path,
                        help="on a host of the target architecture: verify --bootstrap-binary and write its pin marker, then exit")
    parser.add_argument("--bootstrap-binary", type=Path, help="duduclaw-bootstrap to attest with --emit-bootstrap-pin-marker")
    args = parser.parse_args()
    if args.build_only and args.skip_build:
        parser.error("--build-only and --skip-build are mutually exclusive")
    if args.signing_key and (os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS")):
        parser.error("offline isolation signing keys must never be used in CI")
    if bool(args.bootstrap_root) != bool(args.bootstrap_root_signatures) or (args.bootstrap_root and not args.signing_key):
        parser.error("bootstrap inbox requires both root metadata files and the offline target signing key")
    if bool(args.emit_bootstrap_pin_marker) != bool(args.bootstrap_binary):
        parser.error("--emit-bootstrap-pin-marker and --bootstrap-binary are used together")
    version = version_text(workspace_version())
    if args.emit_bootstrap_pin_marker:
        if args.bootstrap_root_sha256 is None:
            parser.error("--emit-bootstrap-pin-marker requires an explicit --bootstrap-root-sha256")
        try:
            pin = parse_bootstrap_pin(args.bootstrap_root_sha256)
        except ValueError as error:
            parser.error(str(error))
        refuse_test_root_environment(os.environ, args.target)
        emit_pin_marker(args.bootstrap_binary, args.target, pin, version, args.emit_bootstrap_pin_marker)
        print(f"Bootstrap pin marker written: {args.emit_bootstrap_pin_marker}")
        return
    try:
        pin = expected_bootstrap_pin(args.bootstrap_root_sha256, os.environ, not args.skip_build)
    except ValueError as error:
        parser.error(str(error))
    if pin is None:
        if args.bootstrap_pin_verified_by is not None:
            parser.error("--bootstrap-pin-verified-by requires --bootstrap-root-sha256")
        print(OMITTED_BOOTSTRAP_LINE)
    else:
        refuse_test_root_environment(os.environ, args.target)
    if not args.skip_build:
        environment = dict(os.environ)
        if pin is not None:
            environment[BOOTSTRAP_PIN_VARIABLE] = pin
        subprocess.run(build_command(args.target, args.build_tool, pin is not None), cwd=ROOT, check=True,
                       env=environment)
    if args.build_only:
        if pin is not None:
            # Check the fresh build at once; packaging re-verifies its own snapshot.
            with tempfile.TemporaryDirectory(prefix="bootstrap-check-") as temporary:
                copy = Path(temporary) / BOOTSTRAP_BINARY
                stage_cargo_binary(release_binary_dir(args.target) / BOOTSTRAP_BINARY, copy)
                verify_bootstrap_binary(copy, args.target, pin, version, args.bootstrap_pin_verified_by)
        return
    binary_dir = release_binary_dir(args.target)
    binaries = archive_binaries(pin is not None)
    # Freeze a single input set for both the tarball and separately signed raw
    # assets; a concurrent build must not make these two outputs disagree.
    with tempfile.TemporaryDirectory(prefix="linux-release-inputs-") as temporary:
        snapshot = Path(temporary)
        snapshot.chmod(0o700)  # mkdtemp already creates 0700; keep it explicit
        for name in binaries:
            stage_cargo_binary(binary_dir / name, snapshot / name)
            subprocess.run([sys.executable, str(ROOT / "scripts/check_glibc.py"),
                            str(snapshot / name)], check=True)
        if pin is not None:
            # The exact bytes that go into the archive (also with --skip-build).
            verify_bootstrap_binary(snapshot / BOOTSTRAP_BINARY, args.target, pin, version,
                                    args.bootstrap_pin_verified_by)
        signed = sign_release(snapshot, args.target, args.output_dir,
                              args.manifest_template, args.signing_key, args.ssh_keygen, args.runtime_artifacts_dir)
        archive = create_archive(snapshot, ROOT / "python", args.target, args.output_dir, binaries=binaries)
        if args.bootstrap_root:
            create_bootstrap_inbox(args.target, args.output_dir, args.bootstrap_root, args.bootstrap_root_signatures)
    print(f"Packaged {archive}; {len(signed)} protected update assets generated.")


if __name__ == "__main__":
    main()
