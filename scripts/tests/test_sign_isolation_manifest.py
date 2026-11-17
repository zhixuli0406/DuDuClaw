"""Offline release-tool tests. All private keys live only in temporary folders."""
import base64
import importlib.util
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT =Path(__file__).resolve().parents[1] / "sign-isolation-manifest.py"
ROOT = SCRIPT.parents[1]
SPEC = importlib.util.spec_from_file_location("sign_isolation", SCRIPT)
SIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIGN)
META_SPEC = importlib.util.spec_from_file_location("isolation_root_metadata",
                                                   SCRIPT.parent / "isolation-root-metadata.py")
META = importlib.util.module_from_spec(META_SPEC)
META_SPEC.loader.exec_module(META)
TEST_SIGNER_BINARY = ROOT / "target/isolation-test-signer/debug/duduclaw-isolation-test-signer"

ED25519_SMALL_ORDER_POINTS = (
    bytes([1]) + bytes(31),  # identity, order 1
    bytes.fromhex("ec" + "ff" * 30 + "7f"),  # y = -1, order 2
    bytes(32),  # y = 0, even x, order 4
    bytes(31) + bytes([0x80]),  # y = 0, odd x (not x = 0), order 4
    *(bytes.fromhex(encoded[:-2] + f"{int(encoded[-2:], 16) ^ sign:02x}")
      for encoded in (
          "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
          "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
      ) for sign in (0, 0x80)),  # both x signs of each order-8 y-coordinate
)
ED25519_ORDINARY_POINTS = (
    bytes.fromhex("58" + "66" * 31),  # base point
    bytes.fromhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
    bytes.fromhex("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"),
)


def find_ssh_keygen():
    for directory in SIGN.TOOL_DIRECTORIES:
        path = os.path.join(directory, "ssh-keygen")
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None


SSH_KEYGEN = find_ssh_keygen()


def generate_key(directory, name):
    key = Path(directory) / name
    subprocess.run([SSH_KEYGEN, "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", str(key)], check=True)
    return key


def root_document(keys, threshold=2, revoked=()):
    """Canonical-ready root metadata over the public halves of `keys` (same key_id rule as the verifier)."""
    roots = [META.parse_public_key(Path(str(key) + ".pub").read_text(), META.ROOT_APPLICATION) for key in keys]
    return {"schema_version": 1, "version": 1, "expires_at_unix": 4_000_000_000, "root_threshold": threshold,
            "root_keys": roots, "target_key": roots[-1], "revoked_key_ids": [list(item) for item in revoked],
            "frozen": False}


def clean_environment(extra=None):
    """The caller's environment without any key-location variable (those are refused)."""
    environment = {name: value for name, value in os.environ.items() if not SIGN.KEY_VARIABLE.fullmatch(name)}
    environment.update(extra or {})
    return environment


def locations(keys):
    """Production key locations: stdin, one per line, key 1 first (never argv or the environment)."""
    return "".join(f"{key}\n" for key in keys)


def run_production(command, keys, extra=None):
    return subprocess.run(command, input=locations(keys), capture_output=True, text=True,
                          env=clean_environment(extra))


def rearmor(blob):
    encoded = base64.b64encode(bytes(blob)).decode("ascii")
    return ("-----BEGIN SSH SIGNATURE-----\n" + "\n".join(encoded[i:i + 70] for i in range(0, len(encoded), 70))
            + "\n-----END SSH SIGNATURE-----\n")


def unarmor(armor):
    return bytearray(base64.b64decode("".join(armor.splitlines()[1:-1])))


def stat_mode(path):
    return path.stat().st_mode & 0o7777


def recording_tool(directory, name, status):
    """A fake tool (python, explicit shebang) that records argv, environment, stdin kind and the
    target of a `-f`/`-s` key link to <name>.json next to itself, then exits with `status`."""
    record = Path(directory) / f"{name}.json"
    tool = Path(directory) / name
    tool.write_text(f"#!{sys.executable} -I\n"
                    "import json, os, sys\n"
                    "argv = sys.argv\n"
                    "link = None\n"
                    "for flag in ('-f', '-s'):\n"
                    "    if flag in argv:\n"
                    "        link = os.readlink(argv[argv.index(flag) + 1])\n"
                    "calls = []\n"
                    f"if os.path.exists({str(record)!r}):\n"
                    f"    calls = json.load(open({str(record)!r}))\n"
                    "calls.append({'argv': argv, 'env': dict(os.environ), 'link': link,\n"
                    "              'stdin_tty': os.isatty(0)})\n"
                    f"json.dump(calls, open({str(record)!r}, 'w'))\n"
                    f"sys.exit({status})\n")
    tool.chmod(0o755)
    return tool, record


class ManifestSigningTests(unittest.TestCase):
    def test_canonical_json_and_ambiguous_input(self):
        self.assertEqual(SIGN.canonical_bytes({"z": {"b": 2, "a": "員工"}, "a": 1}),
                         '{"a":1,"z":{"a":"員工","b":2}}'.encode())
        for raw in ['{"a":1,"a":2}', '{"a":1.0}', '{"a":NaN}']:
            with self.assertRaises(ValueError):
                json.loads(raw, object_pairs_hook=SIGN.unique_object,
                           parse_float=SIGN.reject_number, parse_constant=SIGN.reject_number)

    def test_artifact_digest_size_and_path_rejection(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            data = b"test artifact"
            (directory / "gateway").write_bytes(data)
            document = {"artifacts": [{"id": "gateway", "mode": 0o755,
                                       "install_target": {"kind": "gateway"}}]}
            SIGN.bind_artifacts(document, directory)
            self.assertEqual(document["artifacts"][0]["sha256"], list(hashlib.sha256(data).digest()))
            self.assertEqual(document["artifacts"][0]["size_bytes"], len(data))
            for artifact_id in ("../gateway", ".", "..", "/gateway", "x" * 129):
                document["artifacts"][0]["id"] = artifact_id
                with self.assertRaises(ValueError):
                    SIGN.bind_artifacts(document, directory)

    @unittest.skipUnless(hasattr(__import__("os"), "O_NOFOLLOW"), "requires nofollow open")
    def test_symlink_artifact_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "source").write_bytes(b"data")
            (directory / "gateway").symlink_to("source")
            document = {"artifacts": [{"id": "gateway", "mode": 0o755,
                                       "install_target": {"kind": "gateway"}}]}
            with self.assertRaises(OSError):
                SIGN.bind_artifacts(document, directory)

    def test_policy_template_version_domain_and_launcher_shape(self):
        binding = {"profile": [2] * 32, "slot": "foreground", "seccomp": {"allowed_syscalls": [0, 231, 322]},
                   "seccomp_digest": [3] * 32, "memory_bytes": 1, "tasks": 1, "cpu_quota_percent": 1,
                   "open_files": 64, "file_size_bytes": 1, "timeout_ms": 300_000,
                   "filesystem": ["system_ro", "work"], "egress": False,
                   "self_only": [{"kind": "prlimit64_self_only"}, {"kind": "prctl", "options": [1, 4, 38]}]}
        launcher = {"reserve": {"memory_bytes": 1, "tasks": 1, "cpu_quota_percent": 1},
                    "helpers": [binding], "helper_tmpfs_bytes": 4096}
        def policy(version=2, domain=SIGN.POLICY_DOMAIN_V2, launcher=launcher):
            document = {"version": version, "runtimes": [{}],
                        "native": [{"profile": [1] * 32, "tmpfs_bytes": 4096, "launcher": launcher}]}
            if domain is not None:
                document["domain"] = domain
            return document
        SIGN.validate_policy_document(policy())
        SIGN.validate_policy_document(policy(version=1, domain=None, launcher=None))
        bad = [policy(version=1, domain=None), policy(version=1), policy(domain=None),
               policy(domain="duduclaw-isolation-policy-v1"), policy(version=3)]
        for change in [("timeout_ms", 300_001), ("filesystem", ["work", "home"]), ("filesystem", ["work_recordings"]),
                       ("egress", True), ("slot", "background"), ("self_only", [{"kind": "prctl", "options": [22]}]),
                       ("self_only", [{"kind": "prlimit64_any"}]), ("tasks", 0), ("extra", 1)]:
            wide = dict(binding)
            wide[change[0]] = change[1]
            bad.append(policy(launcher=dict(launcher, helpers=[wide])))
        bad.append(policy(launcher=dict(launcher, helpers=[binding] * 17)))
        bad.append(policy(launcher=dict(launcher, helper_tmpfs_bytes=8192)))
        for document in bad:
            with self.assertRaises(ValueError):
                SIGN.validate_policy_document(document)

    LEGACY = SIGN.CLAUDE_FIXED_PREFIX + ["--max-turns", "50", "--model", "model"] + SIGN.CLAUDE_MCP_SUFFIX
    RANGED = SIGN.CLAUDE_FIXED_PREFIX + ["--model", "model"] + SIGN.CLAUDE_MCP_SUFFIX

    def test_policy_v3_turn_limit_shape(self):
        def claude(limit=None, args=None):
            default = self.RANGED if limit is not None else self.LEGACY
            record = {"runtime": {"kind": "claude"}, "mcp": True,
                      "fixed_args": list(args if args is not None else default)}
            if limit is not None:
                record["turn_limit"] = limit
            return record

        def policy(version=3, domain=SIGN.POLICY_DOMAIN_V3, runtime=None):
            document = {"version": version, "runtimes": [runtime if runtime is not None else claude(
                dict(SIGN.TURN_LIMIT_WORKING_DEFAULT))], "native": [{"profile": [1] * 32, "tmpfs_bytes": 4096}]}
            if domain is not None:
                document["domain"] = domain
            return document
        SIGN.validate_policy_document(policy())
        SIGN.validate_policy_document(policy(runtime=claude()))
        SIGN.validate_policy_document(policy(runtime=claude({"min": 1, "default": 1000, "max": 1000})))
        # v1/v2 without a range are unaffected.
        SIGN.validate_policy_document(policy(version=2, domain=SIGN.POLICY_DOMAIN_V2, runtime=claude()))
        SIGN.validate_policy_document(policy(version=1, domain=None, runtime=claude()))
        bad = [policy(domain=None), policy(domain=SIGN.POLICY_DOMAIN_V2),
               policy(version=2, domain=SIGN.POLICY_DOMAIN_V3),
               policy(version=2, domain=SIGN.POLICY_DOMAIN_V2),
               policy(version=1, domain=None)]
        for limit in [{"min": 0, "default": 50, "max": 200}, {"min": 60, "default": 50, "max": 200},
                      {"min": 1, "default": 50, "max": 40}, {"min": 1, "default": 50, "max": 1001},
                      {"min": 1, "max": 200}, {"min": 1, "default": 50, "max": 200, "step": 1},
                      {"min": True, "default": 50, "max": 200}, {"min": "1", "default": 50, "max": 200}, [1, 50, 200]]:
            bad.append(policy(runtime=claude(limit)))
        wide = dict(SIGN.TURN_LIMIT_WORKING_DEFAULT)
        bad.append(policy(runtime=claude(wide, ["--max-turns", "500", "-p"] + SIGN.CLAUDE_MCP_SUFFIX)))
        bad.append(policy(runtime=claude(wide, ["--max-turns=500"] + SIGN.CLAUDE_MCP_SUFFIX)))
        bad.append(policy(runtime=claude(wide, ["-p"])))
        bad.append(policy(runtime=dict(claude(wide), runtime={"kind": "codex"})))
        bad.append(policy(runtime=dict(claude(wide), mcp=False)))
        for document in bad:
            with self.assertRaises(ValueError):
                SIGN.validate_policy_document(document)

    def test_claude_mcp_step_limit_shape_in_every_version(self):
        """Review round, findings 1-3: the signer refuses what spawnd refuses."""
        codex = {"runtime": {"kind": "codex"}, "mcp": True, "fixed_args": []}

        def policy(version, args, limit=None, has_limit=False):
            record = {"runtime": {"kind": "claude"}, "mcp": True, "fixed_args": list(args)}
            if has_limit or limit is not None:
                record["turn_limit"] = limit
            document = {"version": version, "runtimes": [record, dict(codex)],
                        "native": [{"profile": [1] * 32, "tmpfs_bytes": 4096}, {"profile": [2] * 32, "tmpfs_bytes": 4096}]}
            if version > 1:
                document["domain"] = SIGN.POLICY_DOMAIN_V2 if version == 2 else SIGN.POLICY_DOMAIN_V3
            return document

        def refused(document):
            with self.assertRaises(ValueError):
                SIGN.validate_policy_document(document)

        prefix, suffix, limit = SIGN.CLAUDE_FIXED_PREFIX, SIGN.CLAUDE_MCP_SUFFIX, dict(SIGN.TURN_LIMIT_WORKING_DEFAULT)
        for version in (1, 2, 3):
            SIGN.validate_policy_document(policy(version, self.LEGACY))
            SIGN.validate_policy_document(policy(version, prefix + ["--max-turns", "50"] + suffix))
            # Neither a range nor the fixed flag.
            refused(policy(version, self.RANGED))
            refused(policy(version, prefix + suffix))
            # The fixed flag with another value, duplicated, in `=` spelling or moved.
            for value in ("49", "500", "050", "+50", "", "0"):
                refused(policy(version, prefix + ["--max-turns", value, "--model", "model"] + suffix))
            refused(policy(version, prefix + ["--max-turns", "50", "--max-turns", "50"] + suffix))
            refused(policy(version, prefix + ["--max-turns", "50", "--max-turns=50"] + suffix))
            refused(policy(version, prefix + ["--max-turns=50", "--model", "model"] + suffix))
            refused(policy(version, prefix + ["--model", "model", "--max-turns", "50"] + suffix))
            refused(policy(version, ["--max-turns", "50"] + prefix + suffix))
            # A bare `--` before the suffix.
            refused(policy(version, prefix + ["--max-turns", "50", "--"] + suffix))
            refused(policy(version, ["-p", "--"] + self.LEGACY[1:]))
            # `turn_limit: null` is never "no range"; before v3 the key itself is refused.
            refused(policy(version, self.LEGACY, has_limit=True))
        for version in (1, 2):
            refused(policy(version, self.RANGED, limit))
        SIGN.validate_policy_document(policy(3, self.RANGED, limit))
        SIGN.validate_policy_document(policy(3, prefix + suffix, limit))
        # Both a range and the fixed flag.
        refused(policy(3, self.LEGACY, limit))
        for smuggled in ("--max-turns", "--max-turns=50"):
            refused(policy(3, prefix + [smuggled] + suffix, limit))
        # The insertion point after `--`, after a value-taking option or a
        # model "value" that is an option.
        refused(policy(3, prefix + ["--model", "model", "--"] + suffix, limit))
        refused(policy(3, prefix + ["--model"] + suffix, limit))
        refused(policy(3, prefix[:-1] + suffix, limit))
        for option in sorted(SIGN.CLAUDE_VALUE_OPTIONS):
            refused(policy(3, self.RANGED[:-3] + [option] + suffix, limit))
        for model in ("--", "--allowedTools", "-x", "", " model", "mo\x07del"):
            refused(policy(3, prefix + ["--model", model] + suffix, limit))
            refused(policy(1, prefix + ["--max-turns", "50", "--model", model] + suffix))
        # A non-MCP Claude record has no step limit to enforce.
        SIGN.validate_policy_document(dict(policy(1, []), runtimes=[{"runtime": {"kind": "claude"}, "mcp": False,
                                                                        "fixed_args": []}, dict(codex)]))

    def test_value_options_match_the_canonical_shape(self):
        derived = {option for option, value in zip(self.LEGACY, self.LEGACY[1:])
                   if option.startswith("-") and not value.startswith("-")}
        self.assertEqual(derived, SIGN.CLAUDE_VALUE_OPTIONS)

    @unittest.skipUnless(SSH_KEYGEN, "requires OpenSSH ssh-keygen")
    def test_real_ssh_keygen_round_trip_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            keys = [generate_key(directory, name) for name in ("root-r1", "root-r2")]
            document = root_document(keys)
            template = directory / "template.json"
            template.write_text(json.dumps(document, indent=2))
            output = directory / "root.json"
            command = [sys.executable, "-I", str(SCRIPT), "--kind", "root", "--template", str(template),
                       "--output", str(output), "--key-count", "2", "--ssh-keygen", SSH_KEYGEN]
            result = run_production(command, keys)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload = output.read_bytes()
            self.assertEqual(payload, SIGN.canonical_bytes(document))
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())
            self.assertEqual(len(signatures), 2)
            for key, signature in zip(keys, signatures):
                detached = directory / "signature"
                detached.write_text(signature)
                allowed = directory / "allowed"
                allowed.write_text("root " + Path(str(key) + ".pub").read_text())
                verification = subprocess.run([SSH_KEYGEN, "-Y", "verify", "-f", str(allowed),
                    "-I", "root", "-n", SIGN.NAMESPACES["root"], "-s", str(detached)],
                    input=payload, capture_output=True)
                self.assertEqual(verification.returncode, 0, verification.stderr)
            self.assertNotEqual(run_production(command, keys).returncode, 0)
            self.assertEqual(output.read_bytes(), payload)
            # The written pair passes the standalone check used by release.sh.
            check = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "root", "--check-threshold",
                                    str(output), "--root-sha256", hashlib.sha256(payload).hexdigest(),
                                    "--ssh-keygen", SSH_KEYGEN], capture_output=True, text=True)
            self.assertEqual(check.returncode, 0, check.stderr)
            self.assertIn("every signature verified", check.stdout)

    # ---------------------------------------------------------------- R2: --kind bundle

    def test_bundle_namespace_matches_the_rust_verifier(self):
        source = (SCRIPT.parents[1] / "crates/duduclaw-isolation/src/update.rs").read_text()
        constants = {"root": "ROOT_SSHSIG_NAMESPACE", "release": "RELEASE_SSHSIG_NAMESPACE",
                     "runtime": "RUNTIME_SSHSIG_NAMESPACE", "emergency": "EMERGENCY_SSHSIG_NAMESPACE",
                     "bundle": "MIGRATION_BUNDLE_SSHSIG_NAMESPACE"}
        for kind, constant in constants.items():
            self.assertIn(f'pub const {constant}: &str = "{SIGN.NAMESPACES[kind]}";', source)
        self.assertEqual(len(set(SIGN.NAMESPACES.values())), len(SIGN.NAMESPACES))
        self.assertIn("pub const MAX_MIGRATION_BUNDLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;", source)
        self.assertEqual(SIGN.MAX_BUNDLE_BYTES, 2 * 1024 * 1024 * 1024)
        # The accept set mirrored from sshsig.rs / update.rs.
        self.assertIn(f'pub const ROOT_SK_APPLICATION: &str = "{SIGN.ROOT_SK_APPLICATION}";', source)
        self.assertIn(f'pub const TARGET_SK_APPLICATION: &str = "{SIGN.TARGET_SK_APPLICATION}";', source)
        self.assertIn("pub const REQUIRED_SK_FLAGS: u8 = 0x01 | 0x04;", source)
        self.assertEqual(SIGN.REQUIRED_SK_FLAGS, 0x05)
        sshsig = (SCRIPT.parents[1] / "crates/duduclaw-updated/src/sshsig.rs").read_text()
        self.assertIn('const ED25519: &[u8] = b"ssh-ed25519";', sshsig)
        self.assertIn('const SK_ED25519: &[u8] = b"sk-ssh-ed25519@openssh.com";', sshsig)
        self.assertIn("public.verify_strict(&signed, &signature)", sshsig)
        self.assertIn("if public.is_weak()", sshsig)

    def test_bundle_authorization_binds_tar_digest_size_version_and_arch(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            data = b"ustar bundle bytes" * 100
            tar = directory / "duduclaw-isolated-linux-aarch64-1.83.0.tar"
            tar.write_bytes(data)
            document = SIGN.bundle_authorization(tar, 2_000_000_000, now=1_800_000_000)
            self.assertEqual(document, {
                "schema_version": 1, "product": "duduclaw", "version": {"major": 1, "minor": 83, "patch": 0},
                "arch": "aarch64", "bundle_sha256": list(hashlib.sha256(data).digest()),
                "bundle_size": len(data), "expires_at_unix": 2_000_000_000})
            # Canonical bytes have exactly the BundleAuthorization field set (deny_unknown_fields).
            self.assertEqual(SIGN.canonical_bytes(document)[:24], b'{"arch":"aarch64","bundl')
            for expires in (1_800_000_000, 1_700_000_000, True, 2 ** 63):
                with self.subTest(expires=expires), self.assertRaises(ValueError):
                    SIGN.bundle_authorization(tar, expires, now=1_800_000_000)
            for name in ("duduclaw-isolated-linux-x86_64-1.83.0.tar.gz", "duduclaw-isolated-linux-arm64-1.83.0.tar",
                         "duduclaw-isolated-linux-x86_64-01.83.0.tar", "duduclaw-isolated-linux-x86_64-1.83.tar",
                         "duduclaw-isolated-linux-x86_64-1.83.0-rc1.tar", "isolated-linux-x86_64-1.83.0.tar"):
                renamed = directory / name
                renamed.write_bytes(data)
                with self.subTest(name=name), self.assertRaises(ValueError):
                    SIGN.bundle_authorization(renamed, 2_000_000_000, now=1_800_000_000)
            empty = directory / "duduclaw-isolated-linux-x86_64-1.83.1.tar"
            empty.write_bytes(b"")
            with self.assertRaises(ValueError):
                SIGN.bundle_authorization(empty, 2_000_000_000, now=1_800_000_000)
            linked = directory / "duduclaw-isolated-linux-x86_64-1.83.2.tar"
            linked.symlink_to(tar)
            with self.assertRaises(OSError):
                SIGN.bundle_authorization(linked, 2_000_000_000, now=1_800_000_000)
            hardlinked = directory / "duduclaw-isolated-linux-x86_64-1.83.3.tar"
            os.link(tar, hardlinked)
            with self.assertRaises(ValueError):
                SIGN.bundle_authorization(hardlinked, 2_000_000_000, now=1_800_000_000)

    def test_bundle_kind_arguments_need_a_root_threshold(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            tar = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
            tar.write_bytes(b"bundle")
            template = directory / "template.json"
            template.write_text("{}")
            base = [sys.executable, str(SCRIPT), "--kind", "bundle", "--output", str(directory / "out.json")]
            cases = [
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key-count", "1"], "root threshold"),
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key-count", "4"], "root threshold"),
                (["--bundle", str(tar), "--key-count", "2"], "requires --bundle and --expires-at-unix"),
                (["--template", str(template), "--bundle", str(tar), "--expires-at-unix", "4000000000",
                  "--key-count", "2"], "never a template"),
                (["--bundle", str(tar), "--expires-at-unix", "1", "--key-count", "2"], "expire in the future"),
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key-count", "2"],
                 "requires --root-metadata"),
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key", "a", "--key", "b",
                  "--test-signer", "x", "--ssh-keygen", "/opt/homebrew/bin/ssh-keygen"], "mutually exclusive"),
            ]
            for extra, message in cases:
                with self.subTest(extra=extra):
                    result = run_production(base + extra, [])
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertFalse((directory / "out.json").exists())
            result = subprocess.run([sys.executable, str(SCRIPT), "--kind", "release", "--bundle", str(tar),
                                     "--output", str(directory / "out.json")],
                                    capture_output=True, text=True)
            self.assertIn("belong to --kind bundle", result.stderr)

    def test_production_mode_refuses_key_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            key = directory / "real-hardware-handle"
            key.write_text("not a key")
            template = directory / "template.json"
            template.write_text("{}")
            for kind, extra in (("release", ["--template", str(template), "--artifacts-dir", str(directory)]),
                                ("root", ["--template", str(template)]),
                                ("bundle", ["--bundle", str(directory / "x.tar"), "--expires-at-unix", "4000000000"])):
                with self.subTest(kind=kind):
                    result = run_production([sys.executable, str(SCRIPT), "--kind", kind, *extra,
                                             "--output", str(directory / "out.json"), "--key", str(key)], [])
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("production signing never takes --key", result.stderr)
                    self.assertFalse((directory / "out.json").exists())
            # The TEST signer path keeps --key; production --key-count is not for it.
            result = subprocess.run([sys.executable, str(SCRIPT), "--template", str(template), "--output",
                                     str(directory / "out.json"), "--key-count", "1", "--test-signer", "x"],
                                    capture_output=True, text=True)
            self.assertIn("--test-signer takes --key", result.stderr)

    # ---------------------------------------------------------------- R2-3: key locations only on stdin

    def test_key_locations_come_only_from_stdin_never_the_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            first, second = directory / "r1", directory / "r2"
            first.write_text("k")
            second.write_text("k")

            def read(text, count=2):
                return SIGN.read_key_locations(count, io.BytesIO(text.encode()), interactive=False)
            self.assertEqual(read(f"{first}\n{second}\n"), [first, second])
            original = os.getcwd()
            try:
                os.chdir(directory)
                self.assertEqual(read("r1\n", 1), [first])  # relative locations are made absolute
            finally:
                os.chdir(original)
            refused = [f"{first}\n", f"{first}\n{second}", f"{first}\n{second}\nextra\n", f"{first}\n{first}\n",
                       f"{first}\r\n{second}\n", f"{first}\n{directory / 'missing'}\n", "\n\n",
                       f"{first}\0\n{second}\n"]
            for text in refused:
                with self.subTest(text=text), self.assertRaises(ValueError) as raised:
                    read(text)
                self.assertNotIn(str(directory), str(raised.exception))
            # The environment is never a source; any key-location variable is refused outright.
            for name in ("DUDUCLAW_ISOLATION_SIGNING_KEY_1", "DUDUCLAW_ISOLATION_TARGET_KEY",
                         "DUDUCLAW_ISOLATION_ROOT_KEY_2", "DUDUCLAW_ISOLATION_LEGACY_MINISIGN_KEY"):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    SIGN.refuse_key_variables({name: str(first)})
            SIGN.refuse_key_variables({"DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT": "/x", "PATH": "/usr/bin"})
            template = directory / "template.json"
            template.write_text('{"a":1}')
            result = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "emergency", "--template",
                                     str(template), "--output", str(directory / "out.json")],
                                    input=f"{first}\n", capture_output=True, text=True,
                                    env=dict(clean_environment(), DUDUCLAW_ISOLATION_SIGNING_KEY_1=str(first)))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("never read from the environment", result.stderr)
            self.assertFalse((directory / "out.json").exists())

    def test_child_environment_is_an_explicit_allow_list(self):
        environment = SIGN.child_environment({"PATH": "/evil", "HOME": "/home/x", "SSH_AUTH_SOCK": "/agent",
                                              "DUDUCLAW_ISOLATION_TARGET_KEY": "/k", "SSH_SK_HELPER": "/evil",
                                              "DISPLAY": ":0", "SSH_ASKPASS": "/evil", "TMPDIR": "/private/t",
                                              "LANG": "C.UTF-8", "PYTHONPATH": "/evil", "LD_PRELOAD": "/evil"})
        self.assertEqual(environment, {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": "/home/x",
                                       "TMPDIR": "/private/t", "LANG": "C.UTF-8"})

    def test_tools_are_absolute_or_looked_up_only_in_the_allow_list(self):
        with tempfile.TemporaryDirectory() as temporary:
            tool = Path(temporary) / "ssh-keygen"
            tool.write_text("#!/bin/sh\n")
            tool.chmod(0o755)
            self.assertEqual(SIGN.resolve_tool(str(tool), "--ssh-keygen"), str(tool))
            for value in ("bin/ssh-keygen", "", "duduclaw-no-such-tool", str(tool) + "\n"):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    SIGN.resolve_tool(value, "--ssh-keygen")
            tool.chmod(0o644)
            with self.assertRaises(ValueError):
                SIGN.resolve_tool(str(tool), "--ssh-keygen")
            # A bare name never comes from PATH.
            tool.chmod(0o755)
            with mock.patch.dict(os.environ, {"PATH": temporary}):
                try:
                    resolved = SIGN.resolve_tool("ssh-keygen", "--ssh-keygen")
                except ValueError:
                    resolved = None
            self.assertNotEqual(resolved, str(tool))
            if resolved is not None:
                self.assertIn(os.path.dirname(resolved), SIGN.TOOL_DIRECTORIES)

    def test_ssh_keygen_argv_names_only_the_neutral_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            key = directory / "duduclaw-target-yubikey-handle"
            key.write_text("handle")
            Path(str(key) + ".pub").write_text("public")
            stage = directory / "metadata.json"
            args = SIGN.argparse.Namespace(test_signer=None, ssh_keygen="ssh-keygen", kind="release")
            with SIGN.neutral_key_link(key) as link:
                argv = SIGN.signer_command(args, link, stage)
                self.assertEqual(argv[argv.index("-f") + 1], str(link))
                self.assertEqual(link.name, "signing-key")
                self.assertEqual(os.readlink(link), str(key))
                self.assertEqual(stat_mode(link.parent), 0o700)
                self.assertEqual(sorted(path.name for path in link.parent.iterdir()),
                                 ["signing-key", "signing-key.pub"])
                for part in argv:
                    self.assertNotIn(key.name, part)
                    self.assertNotIn(str(directory), part.replace(str(stage), ""))
            self.assertFalse(link.parent.exists())
            self.assertTrue(key.exists())
            with self.assertRaises(ValueError):
                SIGN.signer_command(args, key, stage)

    def test_ssh_keygen_gets_the_location_only_as_its_link_never_in_argv_or_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            key = directory / "duduclaw-root-r1-handle"
            key.write_text("handle")
            # A software key (its .pub says so): no ssh-sk-helper is needed for it.
            Path(str(key) + ".pub").write_text("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA test\n")
            fake, record = recording_tool(directory, "ssh-keygen", 1)
            template = directory / "template.json"
            template.write_text('{"a":1}')
            result = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "emergency", "--template",
                                     str(template), "--output", str(directory / "out.json"), "--ssh-keygen",
                                     str(fake)], input=f"{key}\n", capture_output=True, text=True,
                                    env=clean_environment({"SSH_AUTH_SOCK": "/agent.sock", "DUDUCLAW_OTHER": "1",
                                                           "SSH_SK_HELPER": "/evil", "PYTHONPATH": "/evil"}))
            self.assertNotEqual(result.returncode, 0)
            calls = json.loads(record.read_text())
            self.assertEqual(len(calls), 1)
            call = calls[0]
            link = Path(call["argv"][call["argv"].index("-f") + 1])
            self.assertEqual(link.name, "signing-key")
            self.assertEqual(call["link"], str(key))  # the location arrived on stdin and became the link
            self.assertNotIn(key.name, json.dumps(call["argv"]))
            self.assertNotIn(key.name, json.dumps(call["env"]))
            names = set(call["env"]) - {"LC_CTYPE", "__CF_USER_TEXT_ENCODING"}
            self.assertLessEqual(names, {"PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "SSH_SK_HELPER"})
            for name in ("SSH_AUTH_SOCK", "DUDUCLAW_OTHER", "PYTHONPATH"):
                self.assertNotIn(name, call["env"])
            # Never the caller's SSH_SK_HELPER: this installation has no helper, so ssh-keygen -Y sign
            # gets a never-created path inside the private key-link directory (it cannot start any).
            self.assertEqual(call["env"]["SSH_SK_HELPER"], str(link.parent / SIGN.NO_SK_HELPER_NAME))
            self.assertEqual(call["env"]["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin")
            self.assertFalse(link.parent.exists())  # removed although signing failed
            self.assertFalse((directory / "out.json").exists())

    def test_legacy_minisign_gets_the_location_only_as_its_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            key = directory / "duduclaw-release.key"
            key.write_text("secret")
            bundle = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
            bundle.write_bytes(b"\0" * 1024)
            output = directory / "out.minisig"
            fake, record = recording_tool(directory, "minisign", 0)
            command = [sys.executable, "-I", str(SCRIPT), "--minisign-sign", str(bundle), "--trusted-comment",
                       "duduclaw v1.83.0 duduclaw-isolated-linux-x86_64-1.83.0.tar", "--output", str(output),
                       "--minisign", str(fake)]
            result = subprocess.run(command, input=f"{key}\n", capture_output=True, text=True,
                                    env=clean_environment({"SSH_AUTH_SOCK": "/agent.sock"}))
            # The fake writes no signature, so nothing is published.
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())
            call = json.loads(record.read_text())[0]
            self.assertEqual(call["argv"][1:3], ["-S", "-s"])
            self.assertEqual(Path(call["argv"][3]).name, "signing-key")
            self.assertEqual(call["link"], str(key))
            self.assertNotIn(str(key), json.dumps(call["argv"]) + json.dumps(call["env"]))
            self.assertNotIn("SSH_AUTH_SOCK", call["env"])
            for extra, message in ((["--key", str(key)], "takes only"), (["--trusted-comment", "two\nlines"],
                                                                          "printable")):
                with self.subTest(extra=extra):
                    refused = subprocess.run(command + extra, input=f"{key}\n", capture_output=True, text=True,
                                             env=clean_environment())
                    self.assertNotEqual(refused.returncode, 0)
                    self.assertIn(message, refused.stderr)

    # ---------------------------------------------------------------- FIDO2: ssh-sk-helper of the selected ssh-keygen

    def fake_installation(self, directory, with_helper=True):
        """<prefix>/bin/ssh-keygen (records its environment, writes a placeholder signature) and,
        optionally, <prefix>/libexec/ssh-sk-helper: an installation like /usr or a Homebrew keg."""
        prefix = directory / "prefix"
        for sub in ("bin", "libexec"):
            (prefix / sub).mkdir(parents=True)
            (prefix / sub).chmod(0o755)
        prefix.chmod(0o755)
        record = directory / "ssh-keygen.json"
        keygen = prefix / "bin/ssh-keygen"
        keygen.write_text(f"#!{sys.executable} -I\nimport json, os, sys\n"
                          f"json.dump({{'argv': sys.argv, 'env': dict(os.environ)}}, open({str(record)!r}, 'w'))\n"
                          "open(sys.argv[-1] + '.sig', 'w').write('placeholder signature')\n")
        keygen.chmod(0o755)
        helper = prefix / "libexec/ssh-sk-helper"
        if with_helper:
            helper.write_text("#!/bin/sh\nexit 1\n")
            helper.chmod(0o755)
        return keygen, helper, record

    def sign_emergency(self, directory, keygen, key, *extra):
        template = directory / "template.json"
        template.write_text('{"a":1}')
        output = directory / "out.json"
        result = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "emergency", "--template",
                                 str(template), "--output", str(output), "--ssh-keygen", str(keygen), *extra],
                                input=f"{key}\n", capture_output=True, text=True,
                                env=clean_environment({"SSH_SK_HELPER": "/evil/ssh-sk-helper"}))
        return result, output

    def test_sk_helper_is_the_selected_installations_and_only_ssh_keygen_sign_gets_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            keygen, helper, record = self.fake_installation(directory)
            key = directory / "duduclaw-target-sk-handle"
            key.write_text("handle")
            Path(str(key) + ".pub").write_text("sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29t x\n")
            self.assertEqual(SIGN.sk_helper_candidates(str(keygen))[0], str(helper))
            result, output = self.sign_emergency(directory, keygen, key)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(output.exists())
            call = json.loads(record.read_text())
            self.assertEqual(call["argv"][1:3], ["-Y", "sign"])
            # Exactly the audited helper next to the selected ssh-keygen, never the caller's value.
            self.assertEqual(call["env"]["SSH_SK_HELPER"], os.path.realpath(helper))
            names = set(call["env"]) - {"LC_CTYPE", "__CF_USER_TEXT_ENCODING"}
            self.assertLessEqual(names, {"PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "SSH_SK_HELPER"})
            # No other child gets it: the shared child environment never carries SSH_SK_HELPER.
            self.assertNotIn("SSH_SK_HELPER", SIGN.child_environment({"SSH_SK_HELPER": "/evil", "HOME": "/h"}))
            # An explicitly given helper must be an audited libexec/ssh-sk-helper.
            output.unlink()
            Path(str(output) + ".signatures.json").unlink()
            result, output = self.sign_emergency(directory, keygen, key, "--ssh-sk-helper", str(keygen))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("--ssh-sk-helper must be the absolute path of libexec/ssh-sk-helper", result.stderr)
            self.assertFalse(output.exists())
            helper.chmod(0o777)  # group/other-writable: the audit refuses it, nothing is skipped
            record.unlink()
            result, output = self.sign_emergency(directory, keygen, key)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing", result.stderr)
            self.assertFalse(record.exists())
            self.assertFalse(output.exists())

    def test_security_key_without_a_helper_in_the_selected_installation_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            keygen, helper, record = self.fake_installation(directory, with_helper=False)
            for name, public in (("sk-handle", "sk-ssh-ed25519@openssh.com AAAA x\n"), ("no-public", None)):
                with self.subTest(key=name):
                    key = directory / name
                    key.write_text("handle")
                    if public is not None:
                        Path(str(key) + ".pub").write_text(public)
                    result, output = self.sign_emergency(directory, keygen, key)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("has no ssh-sk-helper in its installation", result.stderr)
                    self.assertIn(str(helper), result.stderr)
                    self.assertNotIn(name, result.stderr)  # messages never name the key location
                    self.assertFalse(record.exists())  # ssh-keygen never ran
                    self.assertFalse(output.exists())
            self.assertTrue(SIGN.may_be_security_key(directory / "no-public"))
            software = directory / "software"
            Path(str(software) + ".pub").write_text("ssh-ed25519 AAAA x\n")
            self.assertFalse(SIGN.may_be_security_key(software))

    # ---------------------------------------------------------------- target key of the pinned root

    def release_inputs(self, directory):
        template = directory / "release-template.json"
        template.write_text(json.dumps({"artifacts": [{"id": "a", "mode": 0o755, "install_target": {"kind": "gateway"}}]}))
        artifacts = directory / "artifacts"
        artifacts.mkdir()
        (artifacts / "a").write_bytes(b"\x7fELF-placeholder")
        return template, artifacts

    def sign_release_manifest(self, directory, name, key, *extra):
        template, artifacts = (directory / "release-template.json", directory / "artifacts")
        output = directory / f"{name}.json"
        result = run_production([sys.executable, "-I", str(SCRIPT), "--kind", "release", "--template", str(template),
                                 "--artifacts-dir", str(artifacts), "--output", str(output), "--ssh-keygen", SSH_KEYGEN,
                                 *extra], [key])
        return result, output

    @unittest.skipUnless(SSH_KEYGEN, "requires OpenSSH ssh-keygen")
    def test_release_manifest_must_verify_against_the_target_key_of_the_pinned_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            r1, r2, r3 = (generate_key(directory, name) for name in ("r1", "r2", "r3"))
            root = directory / "root.json"
            root.write_bytes(SIGN.canonical_bytes(root_document([r1, r2, r3])))  # target_key = r3
            pin = hashlib.sha256(root.read_bytes()).hexdigest()
            self.release_inputs(directory)
            pinned = ["--root-metadata", str(root), "--root-sha256", pin]
            # A wrong but valid key (here a root key) is refused before anything is written.
            result, output = self.sign_release_manifest(directory, "wrong", r1, *pinned)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not the target key of the pinned root metadata", result.stderr)
            self.assertIn("nothing was written", result.stderr)
            self.assertFalse(output.exists())
            self.assertFalse(Path(str(output) + ".signatures.json").exists())
            # The pin binds the root that names the target key.
            result, output = self.sign_release_manifest(directory, "unpinned", r3, "--root-metadata", str(root),
                                                        "--root-sha256", "0" * 64)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match --root-sha256", result.stderr)
            self.assertFalse(output.exists())
            # The target key: written, and the written files verify again (--check-target).
            result, output = self.sign_release_manifest(directory, "good", r3, *pinned)
            self.assertEqual(result.returncode, 0, result.stderr)

            def check(*extra):
                return subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "release", "--check-target",
                                       str(output), "--ssh-keygen", SSH_KEYGEN, *extra], capture_output=True, text=True)
            valid = check(*pinned)
            self.assertEqual(valid.returncode, 0, valid.stderr)
            self.assertIn("the manifest signature verified", valid.stdout)
            self.assertIn("requires --root-metadata", check().stderr)
            # --artifacts-dir: every bound artifact, under the packager's published name, must have
            # exactly the signed digest and size (scripts/release.sh, right before the upload).
            published = directory / "published"
            published.mkdir()
            manifest = published / "isolated-linux-x86_64-manifest.json"
            shutil.copyfile(output, manifest)
            shutil.copyfile(str(output) + ".signatures.json", str(manifest) + ".signatures.json")
            shutil.copyfile(directory / "artifacts/a", published / "isolated-linux-x86_64-a")

            def check_published(path=manifest):
                return subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "release", "--check-target",
                                       str(path), "--artifacts-dir", str(published), "--ssh-keygen", SSH_KEYGEN,
                                       *pinned], capture_output=True, text=True)
            result = check_published()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("every bound artifact has the signed digest and size", result.stdout)
            (published / "isolated-linux-x86_64-a").write_bytes(b"\x7fELF-placeholdeR")  # same size
            result = check_published()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("differs from the digest or size the signed manifest states", result.stderr)
            (published / "isolated-linux-x86_64-a").write_bytes(b"\x7fELF-placeholder!")
            self.assertNotEqual(check_published().returncode, 0)
            (published / "isolated-linux-x86_64-a").unlink()
            (published / "isolated-linux-x86_64-a").symlink_to(directory / "artifacts/a")
            self.assertNotEqual(check_published().returncode, 0)  # never followed
            (published / "isolated-linux-x86_64-a").unlink()
            self.assertNotEqual(check_published().returncode, 0)
            self.assertIn("named <prefix>manifest.json",
                          check_published(output).stderr)
            # A valid signature of another key over the very same bytes is refused.
            other, other_output = self.sign_release_manifest(directory, "other", r1)
            self.assertEqual(other.returncode, 0, other.stderr)
            self.assertEqual(other_output.read_bytes(), output.read_bytes())
            result = check(*pinned, "--signatures", str(other_output) + ".signatures.json")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not the target key", result.stderr)
            # A flipped byte of the target signature fails ssh-keygen -Y verify.
            armors = json.loads(Path(str(output) + ".signatures.json").read_text())
            blob = unarmor(armors[0])
            blob[-32] ^= 1
            tampered = directory / "tampered.json"
            tampered.write_text(json.dumps([rearmor(blob)]))
            result = check(*pinned, "--signatures", str(tampered))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not verify", result.stderr)
            # A revoked target key is refused.
            revoked = directory / "revoked-root.json"
            revoked.write_bytes(SIGN.canonical_bytes(root_document(
                [r1, r2, r3], revoked=[hashlib.sha256(bytes(root_document([r3])["root_keys"][0]["public_blob"]))
                                       .digest()])))
            self.assertIn("revoked", check("--root-metadata", str(revoked)).stderr)
            # Verification is mandatory.
            with self.assertRaises(ValueError):
                SIGN.check_target_signature(armors, root.read_bytes(), SIGN.NAMESPACES["release"], output.read_bytes(),
                                            None)

    # ---------------------------------------------------------------- R2-5 / finding 3: verified signers

    def test_edwards_point_checks_mirror_the_rust_weak_key_rules(self):
        self.assertEqual(len(set(ED25519_SMALL_ORDER_POINTS)), 8)
        for encoded in (*ED25519_SMALL_ORDER_POINTS, *ED25519_ORDINARY_POINTS):
            with self.subTest(point=encoded.hex()):
                small = encoded in ED25519_SMALL_ORDER_POINTS
                point = SIGN._edwards_point(encoded)
                x, y = point
                self.assertEqual(x & 1, encoded[-1] >> 7)
                self.assertEqual((-x * x + y * y - 1 - SIGN._CURVE_D * x * x * y * y) % SIGN._FIELD, 0)
                for _ in range(3):
                    point = SIGN._edwards_add(point, point)
                self.assertEqual(point == (0, 1), small)
                self.assertEqual(SIGN.is_small_order(encoded), small)

    def test_edwards_decoding_rejects_noncanonical_encodings(self):
        # y >= p, or x = 0 (y = +/-1) with the sign bit set, fail RFC 8032 decoding.
        for y in (SIGN._FIELD, SIGN._FIELD + 1, (1 << 255) - 1):
            for sign in (0, 1):
                encoded = (y | (sign << 255)).to_bytes(32, "little")
                with self.subTest(point=encoded.hex()), self.assertRaises(ValueError):
                    SIGN.is_small_order(encoded)
        for y in (1, SIGN._FIELD - 1):
            encoded = (y | (1 << 255)).to_bytes(32, "little")
            with self.subTest(point=encoded.hex()), self.assertRaises(ValueError):
                SIGN.is_small_order(encoded)

    def test_signature_shape_rejects_all_small_order_keys_and_r(self):
        def ssh_string(value):
            return len(value).to_bytes(4, "big") + value

        def public(point):
            return ssh_string(SIGN.ED25519) + ssh_string(point)

        def inner(point):
            return ssh_string(SIGN.ED25519) + ssh_string(point + (1).to_bytes(32, "little"))

        key = {"algorithm": "ed25519", "application": None}
        ordinary = ED25519_ORDINARY_POINTS[0]
        for point in ED25519_SMALL_ORDER_POINTS:
            with self.subTest(public=point.hex()), self.assertRaisesRegex(ValueError, "weak"):
                SIGN.check_signature_shape(public(point), inner(ordinary), key)
            with self.subTest(r=point.hex()), self.assertRaisesRegex(ValueError, "small-order"):
                SIGN.check_signature_shape(public(ordinary), inner(point), key)
        for point in ED25519_ORDINARY_POINTS:
            with self.subTest(point=point.hex()):
                SIGN.check_signature_shape(public(point), inner(ordinary), key)
                SIGN.check_signature_shape(public(ordinary), inner(point), key)

    def signed_bundle(self, directory, keys, all_keys=None):
        root = directory / "root.json"
        root.write_bytes(SIGN.canonical_bytes(root_document(all_keys or keys)))
        tar = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
        tar.write_bytes(b"\0" * 2048)
        output = Path(str(tar) + ".authorization.json")
        result = run_production([sys.executable, "-I", str(SCRIPT), "--kind", "bundle", "--bundle", str(tar),
                                 "--expires-at-unix", "4000000000", "--output", str(output), "--key-count",
                                 str(len(keys)), "--root-metadata", str(root), "--ssh-keygen", SSH_KEYGEN], keys)
        self.assertEqual(result.returncode, 0, result.stderr)
        return root, output

    def check(self, output, root, *extra):
        return subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "bundle", "--check-threshold",
                               str(output), "--root-metadata", str(root), *extra], capture_output=True, text=True)

    @unittest.skipUnless(SSH_KEYGEN, "requires OpenSSH ssh-keygen")
    def test_check_threshold_verifies_every_signature_cryptographically(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            keys = [generate_key(directory, name) for name in ("r1", "r2")]
            root, output = self.signed_bundle(directory, keys, [*keys, generate_key(directory, "r3")])
            valid = self.check(output, root, "--ssh-keygen", SSH_KEYGEN)
            self.assertEqual(valid.returncode, 0, valid.stderr)
            # --bundle: the tar must be exactly the authorized one (scripts/release.sh, before the upload).
            tar = Path(str(output)[:-len(".authorization.json")])
            result = self.check(output, root, "--bundle", str(tar), "--ssh-keygen", SSH_KEYGEN)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("the bundle is the authorized one", result.stdout)
            tar.write_bytes(b"\0" * 2047 + b"\1")
            result = self.check(output, root, "--bundle", str(tar), "--ssh-keygen", SSH_KEYGEN)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("the bundle differs from the digest, size, version or architecture", result.stderr)
            tar.write_bytes(b"\0" * 2048)
            renamed = directory / "duduclaw-isolated-linux-aarch64-1.83.0.tar"
            shutil.copyfile(tar, renamed)
            result = self.check(output, root, "--bundle", str(renamed), "--ssh-keygen", SSH_KEYGEN)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("the bundle differs", result.stderr)
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())

            def check_with(armors):
                tampered = directory / "tampered.json"
                tampered.write_text(json.dumps(armors))
                return self.check(output, root, "--signatures", str(tampered), "--ssh-keygen", SSH_KEYGEN)
            # One flipped byte in the signature (lowest byte of S: still canonical) fails.
            blob = unarmor(signatures[0])
            blob[-32] ^= 1
            result = check_with([rearmor(blob), signatures[1]])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not verify", result.stderr)
            # A non-canonical S (>= the group order) is refused like ed25519-dalek does.
            blob = unarmor(signatures[0])
            blob[-1] |= 0xF0
            result = check_with([rearmor(blob), signatures[1]])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("non-canonical or small-order", result.stderr)
            # Untouched signatures over OTHER (still canonical) bytes do not verify for this document.
            payload = json.loads(output.read_text())
            payload["expires_at_unix"] = 4000000001
            output.write_bytes(SIGN.canonical_bytes(payload))
            result = self.check(output, root, "--ssh-keygen", SSH_KEYGEN)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not verify", result.stderr)
            # Inner algorithm / length the Rust verifier rejects.
            public, inner = SIGN.parse_signature(signatures[0], SIGN.NAMESPACES["bundle"])
            key = {"algorithm": "ed25519", "application": None}
            SIGN.check_signature_shape(public, inner, key)
            name, offset = SIGN._ssh_string(inner, 0)
            signature, _ = SIGN._ssh_string(inner, offset)
            def inner_blob(algorithm, raw, trailer=b""):
                return (len(algorithm).to_bytes(4, "big") + algorithm + len(raw).to_bytes(4, "big") + raw + trailer)
            for bad, message in ((inner_blob(b"rsa-sha2-512", signature), "algorithm"),
                                 (inner_blob(name, signature[:63]), "length"),
                                 (inner_blob(name, signature, b"\0"), "trailing"),
                                 (inner_blob(SIGN.SK_ED25519, signature, b"\x05\0\0\0\x01"), "algorithm")):
                with self.subTest(message=message), self.assertRaises(ValueError):
                    SIGN.check_signature_shape(public, bad, key)
            for wrong_key in ({"algorithm": "sk_ed25519", "application": SIGN.ROOT_SK_APPLICATION},
                              {"algorithm": "ed25519", "application": SIGN.ROOT_SK_APPLICATION},
                              {"algorithm": "sk_ecdsa_sha2_nistp256", "application": SIGN.ROOT_SK_APPLICATION}):
                with self.subTest(key=wrong_key), self.assertRaises(ValueError):
                    SIGN.check_signature_shape(public, inner, wrong_key)

    def test_security_key_signature_shape_requires_exactly_up_and_uv(self):
        raw = bytes.fromhex("58" + "66" * 31)  # a valid, non-small-order point stands in for the key
        application = SIGN.ROOT_SK_APPLICATION.encode()
        public = (len(SIGN.SK_ED25519).to_bytes(4, "big") + SIGN.SK_ED25519 + (32).to_bytes(4, "big") + raw
                  + len(application).to_bytes(4, "big") + application)
        key = {"algorithm": "sk_ed25519", "application": SIGN.ROOT_SK_APPLICATION}
        signature = raw + (1).to_bytes(32, "little")  # R = a valid point, S = 1 (canonical)

        def inner(flags, counter=b"\0\0\0\x2a", extra=b""):
            return (len(SIGN.SK_ED25519).to_bytes(4, "big") + SIGN.SK_ED25519 + (64).to_bytes(4, "big") + signature
                    + bytes([flags]) + counter + extra)
        SIGN.check_signature_shape(public, inner(0x05), key)
        for flags in (0x00, 0x01, 0x04, 0x07, 0x45):
            with self.subTest(flags=flags), self.assertRaises(ValueError):
                SIGN.check_signature_shape(public, inner(flags), key)
        for bad in (inner(0x05, counter=b"\0\0"), inner(0x05, extra=b"\0")):
            with self.assertRaises(ValueError):
                SIGN.check_signature_shape(public, bad, key)
        with self.assertRaises(ValueError):  # the blob binds another role's application
            SIGN.check_signature_shape(public, inner(0x05), {"algorithm": "sk_ed25519",
                                                             "application": SIGN.TARGET_SK_APPLICATION})

    @unittest.skipUnless(SSH_KEYGEN, "requires OpenSSH ssh-keygen")
    def test_test_mode_signatures_are_verified_by_the_test_signer_never_skipped(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            keys = [generate_key(directory, name) for name in ("r1", "r2")]
            root, output = self.signed_bundle(directory, keys, [*keys, generate_key(directory, "r3")])
            payload = output.read_bytes()
            refusing, refusing_record = recording_tool(directory, "refusing-test-signer", 1)
            result = self.check(output, root, "--test-signer", str(refusing))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not verify (test signer verify)", result.stderr)
            accepting, accepting_record = recording_tool(directory, "accepting-test-signer", 0)
            result = self.check(output, root, "--test-signer", str(accepting))
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = json.loads(accepting_record.read_text())
            self.assertEqual(len(calls), 2)  # every signature, not just enough of them
            expected_ids = sorted(hashlib.sha256(bytes(key["public_blob"])).hexdigest()
                                  for key in root_document(keys)["root_keys"])
            self.assertEqual(sorted(call["argv"][6] for call in calls), expected_ids)
            for call in calls:
                self.assertEqual(call["argv"][1:3], ["verify", call["argv"][2]])
                self.assertEqual(call["argv"][3], "bundle")
                self.assertNotIn("SSH_AUTH_SOCK", call["env"])
            # The signing path verifies TEST signatures the same way (the refusing verifier stops it).
            tar = directory / "duduclaw-isolated-linux-x86_64-1.83.1.tar"
            tar.write_bytes(b"\0" * 2048)
            fake_signer = directory / "fake-signer"
            fake_signer.write_text(f"#!{sys.executable} -I\nimport sys\nif sys.argv[1] == 'verify':\n"
                                   "    sys.exit(1)\n"
                                   f"import subprocess\nsubprocess.run([{SSH_KEYGEN!r}, '-Y', 'sign', '-f', "
                                   "sys.argv[2], '-n', 'duduclaw-migration-bundle-v1@dudustudio', '-O', "
                                   "'hashalg=sha512', sys.argv[4]], check=True, stdin=subprocess.DEVNULL)\n"
                                   "import os\nos.rename(sys.argv[4] + '.sig', sys.argv[5]) "
                                   "if sys.argv[4] + '.sig' != sys.argv[5] else None\n")
            fake_signer.chmod(0o755)
            signed = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "bundle", "--bundle", str(tar),
                                     "--expires-at-unix", "4000000000", "--output", str(tar) + ".authorization.json",
                                     "--root-metadata", str(root), "--key", str(keys[0]), "--key", str(keys[1]),
                                     "--test-signer", str(fake_signer)], capture_output=True, text=True)
            self.assertNotEqual(signed.returncode, 0)
            self.assertIn("does not verify (test signer verify)", signed.stderr)
            self.assertFalse(Path(str(tar) + ".authorization.json").exists())
            self.assertEqual(output.read_bytes(), payload)

    @unittest.skipUnless(TEST_SIGNER_BINARY.is_file(), "build the TEST signer first (scripts/isolation-test-keys.sh)")
    def test_real_test_signer_signatures_verify_and_tampered_ones_do_not(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve()
            for role, name in (("root-sk", "root-r1"), ("root", "root-r2"), ("root", "root-r3"),
                               ("target-sk", "target")):
                subprocess.run([str(TEST_SIGNER_BINARY), "keygen", role, str(directory / name)], check=True,
                               capture_output=True)
            unsigned = directory / "root.unsigned.json"
            subprocess.run([sys.executable, "-I", str(SCRIPT.parent / "isolation-root-metadata.py"),
                            "--root-key", str(directory / "root-r1.pub"), "--root-key", str(directory / "root-r2.pub"),
                            "--root-key", str(directory / "root-r3.pub"), "--target-key", str(directory / "target.pub"),
                            "--expires-at-unix", "4000000000", "--output", str(unsigned)], check=True,
                           capture_output=True)
            root = directory / "root.json"
            result = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "root", "--template", str(unsigned),
                                     "--output", str(root), "--key", str(directory / "root-r1"), "--key",
                                     str(directory / "root-r2"), "--test-signer", str(TEST_SIGNER_BINARY)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            check = [sys.executable, "-I", str(SCRIPT), "--kind", "root", "--check-threshold", str(root),
                     "--test-signer", str(TEST_SIGNER_BINARY)]
            self.assertEqual(subprocess.run(check, capture_output=True).returncode, 0)
            signatures = json.loads(Path(str(root) + ".signatures.json").read_text())
            blob = unarmor(signatures[0])
            blob[-37] ^= 1  # R1 is a simulated security key: S ends 5 bytes before the flags+counter
            tampered = directory / "tampered.json"
            tampered.write_text(json.dumps([rearmor(blob), signatures[1]]))
            result = subprocess.run(check + ["--signatures", str(tampered)], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not verify", result.stderr)

    @unittest.skipUnless(SSH_KEYGEN, "requires OpenSSH ssh-keygen")
    def test_bundle_signers_are_identified_by_the_key_inside_each_signature(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            r1, r2, r3, stranger = (generate_key(directory, name) for name in ("r1", "r2", "r3", "stranger"))
            root = directory / "root.json"
            root.write_bytes(SIGN.canonical_bytes(root_document([r1, r2, r3])))
            pin = hashlib.sha256(root.read_bytes()).hexdigest()
            # The same key under a second path (a hardlink, with its .pub) is still one signer.
            alias = directory / "r1-second-path"
            os.link(r1, alias)
            os.link(Path(str(r1) + ".pub"), Path(str(alias) + ".pub"))
            revoked_root = directory / "revoked-root.json"
            revoked_root.write_bytes(SIGN.canonical_bytes(root_document(
                [r1, r2, r3], revoked=[hashlib.sha256(bytes(root_document([r2])["root_keys"][0]["public_blob"]))
                                       .digest()])))

            def sign(name, keys, root_path=root, expected=pin):
                tar = directory / f"duduclaw-isolated-linux-x86_64-1.83.{name}.tar"
                tar.write_bytes(b"\0" * 2048)
                output = Path(str(tar) + ".authorization.json")
                command = [sys.executable, "-I", str(SCRIPT), "--kind", "bundle", "--bundle", str(tar),
                           "--expires-at-unix", "4000000000", "--output", str(output), "--key-count", str(len(keys)),
                           "--root-metadata", str(root_path), "--ssh-keygen", SSH_KEYGEN]
                if expected is not None:
                    command += ["--root-sha256", expected]
                return run_production(command, keys), output
            for number, (keys, root_path, expected, message) in enumerate((
                    ([r1, alias], root, pin, "same key"),
                    ([r1, stranger], root, pin, "not a root key"),
                    ([r1, r2], revoked_root, None, "revoked"),
                    ([r1, r2], root, "0" * 64, "does not match --root-sha256"))):
                with self.subTest(message=message):
                    result, output = sign(str(number), keys, root_path, expected)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(output.exists())
                    self.assertFalse(Path(str(output) + ".signatures.json").exists())
            result, output = sign("9", [r1, r2])
            self.assertEqual(result.returncode, 0, result.stderr)
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())
            verify = SIGN.ssh_keygen_verifier(SSH_KEYGEN)
            self.assertEqual(SIGN.check_root_threshold(signatures, root.read_bytes(), SIGN.NAMESPACES["bundle"],
                                                       output.read_bytes(), verify),
                             sorted(hashlib.sha256(bytes(key["public_blob"])).hexdigest()
                                    for key in root_document([r1, r2])["root_keys"]))
            # Verification is mandatory: no verifier, no acceptance.
            with self.assertRaises(ValueError):
                SIGN.check_root_threshold(signatures, root.read_bytes(), SIGN.NAMESPACES["bundle"],
                                          output.read_bytes(), None)
            # The release.sh re-check of the written files, and its refusals.
            def check(*extra):
                return subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "bundle", "--check-threshold",
                                       str(output), "--ssh-keygen", SSH_KEYGEN, *extra], capture_output=True, text=True)
            self.assertEqual(check("--root-metadata", str(root), "--root-sha256", pin).returncode, 0)
            self.assertNotEqual(check("--root-metadata", str(revoked_root)).returncode, 0)
            self.assertNotEqual(check().returncode, 0)
            duplicated = directory / "duplicated.json"
            duplicated.write_text(json.dumps([signatures[0], signatures[0]]))
            result = check("--root-metadata", str(root), "--signatures", str(duplicated))
            self.assertIn("same key", result.stderr)
            # Wrong namespace: a root-metadata signature is not a bundle authorization.
            self.assertRaises(ValueError, SIGN.check_root_threshold, signatures, root.read_bytes(),
                              SIGN.NAMESPACES["root"], output.read_bytes(), verify)
            single = directory / "single.json"
            single.write_text(json.dumps(signatures[:1]))
            self.assertIn("threshold", check("--root-metadata", str(root), "--signatures", str(single)).stderr)

    @unittest.skipUnless(SSH_KEYGEN, "requires OpenSSH ssh-keygen")
    def test_real_ssh_keygen_bundle_threshold_round_trip(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            keys = [generate_key(directory, name) for name in ("r1", "r2")]
            root = directory / "root.json"
            root.write_bytes(SIGN.canonical_bytes(root_document([*keys, generate_key(directory, "r3")])))
            tar = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
            tar.write_bytes(b"\0" * 2048)
            output = Path(str(tar) + ".authorization.json")
            result = run_production([sys.executable, str(SCRIPT), "--kind", "bundle", "--bundle", str(tar),
                                     "--expires-at-unix", "4000000000", "--output", str(output),
                                     "--key-count", "2", "--root-metadata", str(root), "--ssh-keygen", SSH_KEYGEN],
                                    keys)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload = output.read_bytes()
            self.assertEqual(json.loads(payload)["bundle_sha256"], list(hashlib.sha256(b"\0" * 2048).digest()))
            self.assertEqual(result.stdout.strip(), hashlib.sha256(payload).hexdigest())
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())
            self.assertEqual(len(signatures), 2)
            for key, signature in zip(keys, signatures):
                detached = directory / "signature"
                detached.write_text(signature)
                allowed = directory / "allowed"
                allowed.write_text("root " + Path(str(key) + ".pub").read_text())
                for namespace, ok in ((SIGN.NAMESPACES["bundle"], True), (SIGN.NAMESPACES["root"], False)):
                    verification = subprocess.run([SSH_KEYGEN, "-Y", "verify", "-f", str(allowed), "-I", "root",
                        "-n", namespace, "-s", str(detached)], input=payload, capture_output=True)
                    self.assertEqual(verification.returncode == 0, ok, verification.stderr)


if __name__ == "__main__":
    unittest.main()
