"""Offline release-tool tests. All private keys live only in temporary folders."""
import importlib.util
import hashlib
import json
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
    def test_real_ssh_keygen_round_trip_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            key = directory / "key"
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
            template = directory / "template.json"
            template.write_text('{"z":2,"a":1}')
            output = directory / "root.json"
            command = [sys.executable, str(SCRIPT), "--kind", "root", "--template", str(template),
                       "--output", str(output), "--key", str(key)]
            subprocess.run(command, check=True, capture_output=True)
            payload = output.read_bytes()
            self.assertEqual(payload, b'{"a":1,"z":2}')
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())
            detached = directory / "signature"
            detached.write_text(signatures[0])
            allowed = directory / "allowed"
            allowed.write_text("root " + Path(str(key) + ".pub").read_text())
            verification = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed),
                "-I", "root", "-n", SIGN.NAMESPACES["root"], "-s", str(detached)],
                input=payload, capture_output=True)
            self.assertEqual(verification.returncode, 0, verification.stderr)
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(output.read_bytes(), payload)


if __name__ == "__main__":
    unittest.main()
