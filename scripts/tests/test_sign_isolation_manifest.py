"""Offline release-tool tests. All private keys live only in temporary folders."""
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "sign-isolation-manifest.py"
SPEC = importlib.util.spec_from_file_location("sign_isolation", SCRIPT)
SIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIGN)
META_SPEC = importlib.util.spec_from_file_location("isolation_root_metadata",
                                                   SCRIPT.parent / "isolation-root-metadata.py")
META = importlib.util.module_from_spec(META_SPEC)
META_SPEC.loader.exec_module(META)


def generate_key(directory, name):
    key = Path(directory) / name
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", str(key)], check=True)
    return key


def root_document(keys, threshold=2, revoked=()):
    """Canonical-ready root metadata over the public halves of `keys` (same key_id rule as the verifier)."""
    roots = [META.parse_public_key(Path(str(key) + ".pub").read_text(), META.ROOT_APPLICATION) for key in keys]
    return {"schema_version": 1, "version": 1, "expires_at_unix": 4_000_000_000, "root_threshold": threshold,
            "root_keys": roots, "target_key": roots[-1], "revoked_key_ids": [list(item) for item in revoked],
            "frozen": False}


def production_environment(keys, extra=None):
    """Key locations only through DUDUCLAW_ISOLATION_SIGNING_KEY_<n> (never argv)."""
    environment = {name: value for name, value in os.environ.items()
                   if not name.startswith("DUDUCLAW_ISOLATION_SIGNING_KEY_")}
    for index, key in enumerate(keys, 1):
        environment[SIGN.KEY_ENVIRONMENT.format(index)] = str(key)
    environment.update(extra or {})
    return environment


def stat_mode(path):
    return path.stat().st_mode & 0o7777


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

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
    def test_policy_signing_validates_bound_bytes_before_accessing_keys(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            key = generate_key(directory, "target")
            artifact = directory / "runtime-policy"
            template = directory / "template.json"
            template.write_text(json.dumps({"artifacts": [{
                "id": artifact.name, "mode": 0o444,
                "install_target": {"kind": "runtime_entry", "registry_id": "isolation-policy-v1"}}]}))
            runtime = {"runtime": {"kind": "claude"}, "mcp": True,
                       "fixed_args": list(self.RANGED), "turn_limit": dict(SIGN.TURN_LIMIT_WORKING_DEFAULT)}
            policy = {"domain": SIGN.POLICY_DOMAIN_V3, "version": 3, "runtimes": [runtime],
                      "native": [{"profile": [1] * 32, "tmpfs_bytes": 4096}]}
            for kind in ("release", "runtime"):
                output = directory / (kind + ".json")
                command = [sys.executable, "-I", str(SCRIPT), "--kind", kind, "--template", str(template),
                           "--artifacts-dir", str(directory), "--output", str(output)]
                for version, domain, arguments, limit in (
                        (3, SIGN.POLICY_DOMAIN_V3, self.LEGACY, dict(SIGN.TURN_LIMIT_WORKING_DEFAULT)),
                        (3, SIGN.POLICY_DOMAIN_V3, self.RANGED, None),
                        (2, SIGN.POLICY_DOMAIN_V2, self.RANGED, dict(SIGN.TURN_LIMIT_WORKING_DEFAULT)),
                        (3, SIGN.POLICY_DOMAIN_V3, self.RANGED, {"min": 1, "default": 50, "max": 1001})):
                    with self.subTest(kind=kind, version=version, arguments=arguments, limit=limit):
                        invalid = dict(policy, version=version, domain=domain,
                                       runtimes=[dict(runtime, fixed_args=arguments, turn_limit=limit)])
                        artifact.write_bytes(SIGN.canonical_bytes(invalid))
                        result = subprocess.run(command, capture_output=True, text=True,
                                                env=production_environment([directory / "missing-key"]))
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("turn_limit", result.stderr)
                        self.assertNotIn("is not a file", result.stderr)
                        self.assertFalse(output.exists())
                        self.assertFalse(Path(str(output) + ".signatures.json").exists())
                artifact.write_bytes(SIGN.canonical_bytes(policy))
                result = subprocess.run(command, capture_output=True, text=True, env=production_environment([key]))
                self.assertEqual(result.returncode, 0, result.stderr)
                payload = output.read_bytes()
                bound = json.loads(payload)["artifacts"][0]
                self.assertEqual(bound["sha256"], list(hashlib.sha256(artifact.read_bytes()).digest()))
                self.assertEqual(bound["size_bytes"], artifact.stat().st_size)
                signature = directory / "signature"
                signature.write_text(json.loads(Path(str(output) + ".signatures.json").read_text())[0])
                allowed = directory / "allowed"
                allowed.write_text("target " + Path(str(key) + ".pub").read_text())
                verified = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed), "-I", "target",
                                           "-n", SIGN.NAMESPACES[kind], "-s", str(signature)],
                                          input=payload, capture_output=True)
                self.assertEqual(verified.returncode, 0, verified.stderr)

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
    def test_real_ssh_keygen_round_trip_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            keys = [generate_key(directory, name) for name in ("root-r1", "root-r2")]
            document = root_document(keys)
            template = directory / "template.json"
            template.write_text(json.dumps(document, indent=2))
            output = directory / "root.json"
            command = [sys.executable, "-I", str(SCRIPT), "--kind", "root", "--template", str(template),
                       "--output", str(output), "--key-count", "2"]
            result = subprocess.run(command, capture_output=True, text=True, env=production_environment(keys))
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
                verification = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed),
                    "-I", "root", "-n", SIGN.NAMESPACES["root"], "-s", str(detached)],
                    input=payload, capture_output=True)
                self.assertEqual(verification.returncode, 0, verification.stderr)
            self.assertNotEqual(subprocess.run(command, capture_output=True,
                                               env=production_environment(keys)).returncode, 0)
            self.assertEqual(output.read_bytes(), payload)
            # The written pair passes the standalone check used by release.sh.
            check = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "root", "--check-threshold",
                                    str(output), "--root-sha256", hashlib.sha256(payload).hexdigest()],
                                   capture_output=True, text=True)
            self.assertEqual(check.returncode, 0, check.stderr)

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
                    result = subprocess.run(base + extra, capture_output=True, text=True,
                                            env=production_environment([]))
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
                    result = subprocess.run([sys.executable, str(SCRIPT), "--kind", kind, *extra,
                                             "--output", str(directory / "out.json"), "--key", str(key)],
                                            capture_output=True, text=True, env=production_environment([]))
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("production signing never takes --key", result.stderr)
                    self.assertFalse((directory / "out.json").exists())
            # The TEST signer path keeps --key; production --key-count is not for it.
            result = subprocess.run([sys.executable, str(SCRIPT), "--template", str(template), "--output",
                                     str(directory / "out.json"), "--key-count", "1", "--test-signer", "x"],
                                    capture_output=True, text=True)
            self.assertIn("--test-signer takes --key", result.stderr)

    def test_key_locations_come_from_the_environment_or_the_prompt(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            first, second = directory / "r1", directory / "r2"
            first.write_text("k")
            second.write_text("k")
            environment = {SIGN.KEY_ENVIRONMENT.format(1): str(first)}
            prompts = []

            def prompt(text):
                prompts.append(text)
                return str(second)
            self.assertEqual(SIGN.key_locations(2, environment, prompt), [first, second])
            self.assertEqual(len(prompts), 1)
            self.assertNotIn(str(first), prompts[0])
            refused = [({}, lambda _: ""), ({}, lambda _: str(directory / "missing")),
                       ({SIGN.KEY_ENVIRONMENT.format(1): str(first), SIGN.KEY_ENVIRONMENT.format(2): str(first)},
                        prompt),
                       ({SIGN.KEY_ENVIRONMENT.format(1): str(first), SIGN.KEY_ENVIRONMENT.format(3): str(second)},
                        prompt),
                       ({SIGN.KEY_ENVIRONMENT.format(1): str(first) + "\n"}, prompt)]
            for environ, answer in refused:
                with self.subTest(environ=environ), self.assertRaises(ValueError) as raised:
                    SIGN.key_locations(2, environ, answer)
                self.assertNotIn(str(directory), str(raised.exception))

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

    def test_signer_process_argv_never_contains_the_key_location(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            key = directory / "duduclaw-root-r1-handle"
            key.write_text("handle")
            log = directory / "argv.log"
            fake = directory / "fake-ssh-keygen"
            fake.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$FAKE_ARGV_LOG"\nexit 1\n')
            fake.chmod(0o755)
            template = directory / "template.json"
            template.write_text('{"a":1}')
            result = subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "emergency", "--template",
                                     str(template), "--output", str(directory / "out.json"), "--ssh-keygen",
                                     str(fake)], capture_output=True, text=True,
                                    env=production_environment([key], {"FAKE_ARGV_LOG": str(log)}))
            self.assertNotEqual(result.returncode, 0)
            argv = log.read_text().splitlines()
            link = Path(argv[argv.index("-f") + 1])
            self.assertEqual(link.name, "signing-key")
            self.assertNotIn(key.name, log.read_text())
            self.assertFalse(link.parent.exists())  # removed although signing failed
            self.assertFalse((directory / "out.json").exists())

    # ---------------------------------------------------------------- R2-5: signers by embedded key

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
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
                           "--root-metadata", str(root_path)]
                if expected is not None:
                    command += ["--root-sha256", expected]
                return subprocess.run(command, capture_output=True, text=True,
                                      env=production_environment(keys)), output
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
            self.assertEqual(SIGN.check_root_threshold(signatures, root.read_bytes(), SIGN.NAMESPACES["bundle"]),
                             sorted(hashlib.sha256(bytes(key["public_blob"])).hexdigest()
                                    for key in root_document([r1, r2])["root_keys"]))
            # The release.sh re-check of the written files, and its refusals.
            def check(*extra):
                return subprocess.run([sys.executable, "-I", str(SCRIPT), "--kind", "bundle", "--check-threshold",
                                       str(output), *extra], capture_output=True, text=True)
            self.assertEqual(check("--root-metadata", str(root), "--root-sha256", pin).returncode, 0)
            self.assertNotEqual(check("--root-metadata", str(revoked_root)).returncode, 0)
            self.assertNotEqual(check().returncode, 0)
            duplicated = directory / "duplicated.json"
            duplicated.write_text(json.dumps([signatures[0], signatures[0]]))
            result = check("--root-metadata", str(root), "--signatures", str(duplicated))
            self.assertIn("same key", result.stderr)
            # Wrong namespace: a root-metadata signature is not a bundle authorization.
            self.assertRaises(ValueError, SIGN.check_root_threshold, signatures, root.read_bytes(),
                              SIGN.NAMESPACES["root"])
            single = directory / "single.json"
            single.write_text(json.dumps(signatures[:1]))
            self.assertIn("threshold", check("--root-metadata", str(root), "--signatures", str(single)).stderr)

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
    def test_real_ssh_keygen_bundle_threshold_round_trip(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            keys = [generate_key(directory, name) for name in ("r1", "r2")]
            root = directory / "root.json"
            root.write_bytes(SIGN.canonical_bytes(root_document([*keys, generate_key(directory, "r3")])))
            tar = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
            tar.write_bytes(b"\0" * 2048)
            output = Path(str(tar) + ".authorization.json")
            result = subprocess.run([sys.executable, str(SCRIPT), "--kind", "bundle", "--bundle", str(tar),
                                     "--expires-at-unix", "4000000000", "--output", str(output),
                                     "--key-count", "2", "--root-metadata", str(root)],
                                    capture_output=True, text=True, env=production_environment(keys))
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
                    verification = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed), "-I", "root",
                        "-n", namespace, "-s", str(detached)], input=payload, capture_output=True)
                    self.assertEqual(verification.returncode == 0, ok, verification.stderr)


if __name__ == "__main__":
    unittest.main()
