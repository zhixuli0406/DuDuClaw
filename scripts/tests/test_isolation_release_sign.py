"""release.sh isolation-sign and scripts/isolation-test-keys.sh fail closed (plan R2 §4.2).

Nothing here touches a key, the network or GitHub: every case stops at a
refusal before the first download or signature.
"""
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RELEASE = ROOT / "scripts/release.sh"
TEST_KEYS = ROOT / "scripts/isolation-test-keys.sh"
PIN_FILE = ROOT / "release/bootstrap-root.sha256"
SPEC = importlib.util.spec_from_file_location("linux_release", ROOT / "scripts/package-linux-release.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)
VERSION = PACKAGE.version_text(PACKAGE.workspace_version())


def release_text():
    return RELEASE.read_text()


def isolation_function():
    text = release_text()
    start = text.index("run_isolation_sign() {")
    return text[start:text.index("\n}\n", start)]


class IsolationSignTests(unittest.TestCase):
    def run_sign(self, *args, env=None, stdin=subprocess.DEVNULL):
        environment = {key: value for key, value in os.environ.items() if key not in ("CI", "GITHUB_ACTIONS")}
        environment.update(env or {})
        return subprocess.run(["bash", str(RELEASE), "isolation-sign", *args], cwd=ROOT, env=environment,
                              stdin=stdin, capture_output=True, text=True, timeout=60)

    def required(self):
        arguments = ["--target-key", "target-key", "--root-metadata", "root.json",
                     "--root-signatures", "root.json.signatures.json", "--output-dir", "/nonexistent/out"]
        if VERSION in release_text().split('ISOLATION_MIGRATION_BUNDLE_VERSIONS="', 1)[1].split('"', 1)[0].split():
            arguments += ["--migration-bundle", "--root-key", "r1", "--root-key", "r2"]
        return arguments

    def test_help_documents_every_step_and_the_secret_rules(self):
        result = self.run_sign("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        for needle in ("PRODUCTION <pin>", "Q6", "ONLY v1.83.0", "--migration-bundle", "BundleAuthorization",
                       "SHA256SUMS-isolation.txt", "type UPLOAD", "without replacing any asset",
                       "never copies, prints or logs a key", "[external] gate", "Never in CI"):
            self.assertIn(needle, result.stdout)
        usage = subprocess.run(["bash", str(RELEASE)], cwd=ROOT, capture_output=True, text=True)
        self.assertIn("isolation-sign --help", usage.stdout)

    def test_refused_in_ci_before_anything_else(self):
        for variable in ("CI", "GITHUB_ACTIONS"):
            result = self.run_sign(*self.required(), env={variable: "true"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("never runs in CI", result.stderr)

    def test_migration_bundle_is_an_explicit_q6_gated_step(self):
        self.assertIn('ISOLATION_MIGRATION_BUNDLE_VERSIONS="1.83.0"', release_text())
        if VERSION == "1.83.0":
            result = self.run_sign("--target-key", "k", "--root-metadata", "r", "--root-signatures", "s",
                                   "--output-dir", "/nonexistent/out")
            self.assertIn("is the migration-bundle release", result.stderr)
        else:
            result = self.run_sign("--migration-bundle", "--root-key", "r1", "--root-key", "r2")
            self.assertIn("owner decision Q6", result.stderr)
        self.assertNotEqual(result.returncode, 0)
        # The root threshold, the bundle tar and its minisign signature only
        # happen inside the `migration_bundle == 1` branch.
        body = isolation_function()
        branch = body[body.index('if [[ "$migration_bundle" == "1" ]]; then\n            # 3.'):]
        for needle in ("--migration-bundle-from", "--kind bundle", "minisign -S"):
            self.assertEqual(body.count(needle), 1, needle)
            self.assertIn(needle, branch)

    def test_arguments_and_non_interactive_runs_are_refused(self):
        bundle_release = "--migration-bundle" in self.required()
        cases = [
            ((), "is the migration-bundle release" if bundle_release else "are required"),
            (("--bogus",), "unknown isolation-sign option"),
            ((*self.required(), "--manifest-valid-days", "0"), "validity"),
            ((*self.required(), "--authorization-valid-days", "1831"), "validity"),
            ((*self.required(),), "interactive terminal"),
        ]
        if "--migration-bundle" not in self.required():
            cases.append(((*self.required(), "--root-key", "r1"), "only used with --migration-bundle"))
        for arguments, message in cases:
            with self.subTest(arguments=arguments):
                result = self.run_sign(*arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    @unittest.skipIf(PIN_FILE.exists(), "the production pin exists; the absent-pin refusal no longer applies")
    def test_absent_production_pin_means_nothing_can_be_signed(self):
        controller, terminal = os.openpty()
        try:
            result = self.run_sign(*self.required(), stdin=terminal)
        finally:
            os.close(controller)
            os.close(terminal)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("there is no production root pin", result.stderr)
        self.assertFalse(Path("/nonexistent/out").exists())

    def test_no_secret_is_copied_printed_or_logged_and_uploads_are_checked(self):
        body = isolation_function()
        self.assertNotRegex(body, r"(?m)^\s*set -x")
        # Key paths only ever reach ssh-keygen / minisign / the signers as arguments.
        for variable in ("target_key", "root_keys", "legacy_minisign_key"):
            self.assertNotRegex(body, r"(?m)^\s*(cat|cp|mv|echo|printf|tee)\b[^\n]*\$\{?" + variable)
        self.assertIn('ISOLATION_WORK="$(mktemp -d', body)
        self.assertIn("trap 'rm -rf -- \"$ISOLATION_WORK\"' EXIT", body)
        self.assertIn('if [[ ! -t 0 ]]; then', body)
        self.assertNotIn("--clobber", body)
        self.assertIn('gh release upload "$tag" --repo "$ISOLATION_REPO" "${uploads[@]}"', body)
        self.assertIn('if [[ "$answer" != "UPLOAD" ]]; then', body)
        # Re-hash before upload and read every asset back afterwards.
        self.assertLess(body.index("changed after it was recorded"), body.index("gh release upload"))
        self.assertLess(body.index("gh release upload"), body.index("differs from the signed bytes"))
        # The released verifier must print exactly PRODUCTION <pin> (layer 2), and
        # the published sums are computed from the bytes uploaded (layer 3).
        self.assertIn('if [[ "$probe_output" != "PRODUCTION $pin" ]]; then', body)
        self.assertIn('hashes+=("$(isolation_sha256 "${uploads[$index]}")")', body)
        self.assertIn("--from-release-archive", body)
        self.assertNotIn("--test-signer", body)
        self.assertNotIn("isolation-test-keys", body)

    def test_dry_run_line_points_to_the_offline_step(self):
        text = release_text()
        self.assertNotIn("F metadata is signed offline only", text)
        self.assertIn("isolation-sign --help (owner present; migration bundle only for:", text)


class IsolationTestKeysTests(unittest.TestCase):
    def run_kit(self, *args):
        return subprocess.run(["bash", str(TEST_KEYS), *args], cwd=ROOT, capture_output=True, text=True, timeout=60)

    def test_help_and_unknown_command(self):
        result = self.run_kit("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("TEST-ONLY", result.stdout)
        self.assertIn("worthless outside", result.stdout)
        self.assertEqual(self.run_kit().returncode, 2)
        self.assertEqual(self.run_kit("sign").returncode, 2)

    def test_output_inside_the_repository_is_refused(self):
        target = ROOT / "release" / "isolation-test-keys-must-not-exist"
        result = self.run_kit("keys", "--out", str(target), "--signer", sys.executable)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside the repository", result.stderr)
        self.assertFalse(target.exists())
        with tempfile.TemporaryDirectory() as temporary:
            existing = Path(temporary)
            result = self.run_kit("keys", "--out", str(existing), "--signer", sys.executable)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must not exist", result.stderr)

    def test_bundle_requires_a_test_key_set(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = self.run_kit("bundle", "--keys", temporary, "--archive", "x.tar.gz",
                                  "--target", "x86_64-unknown-linux-gnu", "--signer", sys.executable)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is not a key set", result.stderr)

    def test_kit_only_uses_test_keys_and_production_code_paths(self):
        text = TEST_KEYS.read_text()
        self.assertIn("set -euo pipefail", text)
        self.assertNotIn("eval", text)
        for forbidden in ("release/bootstrap-root.sha256", "--ssh-keygen", "gh release", "--bootstrap-root-sha256",
                          "--bootstrap-root-pin-file"):
            self.assertNotIn(forbidden, text)
        invocations = re.findall(r'"\$ROOT/scripts/sign-isolation-manifest\.py"(?:[^\n]*\\\n)*[^\n]*', text)
        self.assertEqual(len(invocations), 2)  # root.json and the BundleAuthorization
        for invocation in invocations:
            self.assertIn("--test-signer", invocation)
        packager = re.findall(r'"\$ROOT/scripts/package-linux-release\.py"(?:[^\n]*\\\n)*[^\n]*', text)
        self.assertTrue(any("--from-release-archive" in call and "--test-signer" in call for call in packager))
        self.assertIn("--features test-signer --bin duduclaw-isolation-test-signer", text)
        self.assertIn('CARGO_TARGET_DIR="$ROOT/target/isolation-test-signer"', text)


if __name__ == "__main__":
    unittest.main()
