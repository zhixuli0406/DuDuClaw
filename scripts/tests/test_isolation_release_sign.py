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


def shell_function(name):
    """The complete text of one top-level function of release.sh."""
    text = release_text()
    start = text.index(f"\n{name}() {{\n") + 1
    return text[start:text.index("\n}\n", start) + 3]


def isolation_section():
    text = release_text()
    return text[text.index("# --- isolation-sign:"):text.index("# --- Arg parsing")]


def run_functions(names, command, cwd=ROOT, env=None):
    script = "set -euo pipefail\n" + "".join(shell_function(name) for name in names) + command
    return subprocess.run(["bash", "-c", script], cwd=cwd, env=env, capture_output=True, text=True, timeout=60)


DIGEST = "ubuntu@sha256:" + "ab" * 32


class IsolationSignTests(unittest.TestCase):
    def run_sign(self, *args, env=None, stdin=subprocess.DEVNULL):
        environment = {key: value for key, value in os.environ.items() if key not in ("CI", "GITHUB_ACTIONS")}
        environment.update(env or {})
        return subprocess.run(["bash", str(RELEASE), "isolation-sign", *args], cwd=ROOT, env=environment,
                              stdin=stdin, capture_output=True, text=True, timeout=60)

    def required(self):
        # Key locations are never arguments (R2-3): they come from DUDUCLAW_ISOLATION_* or the prompt.
        arguments = ["--root-metadata", "root.json",
                     "--root-signatures", "root.json.signatures.json", "--output-dir", "/nonexistent/out"]
        if VERSION in release_text().split('ISOLATION_MIGRATION_BUNDLE_VERSIONS="', 1)[1].split('"', 1)[0].split():
            arguments += ["--migration-bundle"]
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
            result = self.run_sign("--root-metadata", "r", "--root-signatures", "s",
                                   "--output-dir", "/nonexistent/out")
            self.assertIn("is the migration-bundle release", result.stderr)
        else:
            result = self.run_sign("--migration-bundle")
            self.assertIn("owner decision Q6", result.stderr)
        self.assertNotEqual(result.returncode, 0)
        # The root threshold, the bundle tar and its minisign signature only
        # happen inside the `migration_bundle == 1` branch.
        body = isolation_function()
        branch = body[body.index('if [[ "$migration_bundle" == "1" ]]; then\n            # 3.'):]
        for needle in ("--migration-bundle-from", "--kind bundle --bundle", "--kind bundle --check-threshold",
                       "isolation_minisign_sign"):
            self.assertEqual(body.count(needle), 1, needle)
            self.assertIn(needle, branch)
        self.assertNotIn("minisign -S", body)
        self.assertEqual(shell_function("isolation_minisign_sign").count("minisign -S"), 1)
        # R2-5: the embedded-key threshold check runs (and deletes on failure) before
        # anything of the bundle is added to the upload list.
        check = branch.index("--kind bundle --check-threshold")
        self.assertLess(check, branch.index('rm -f -- "$authorization" "$authorization.signatures.json"'))
        self.assertLess(check, branch.index("uploads+="))

    def test_arguments_and_non_interactive_runs_are_refused(self):
        bundle_release = "--migration-bundle" in self.required()
        cases = [
            ((), "is the migration-bundle release" if bundle_release else "are required"),
            (("--bogus",), "unknown isolation-sign option"),
            ((*self.required(), "--manifest-valid-days", "0"), "validity"),
            ((*self.required(), "--authorization-valid-days", "1831"), "validity"),
            ((*self.required(),), "interactive terminal"),
        ]
        for option in ("--target-key", "--root-key", "--legacy-minisign-key"):
            cases.append(((*self.required(), option, "/some/key"), "key locations are never command-line arguments"))
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
        # Key locations are never copied or printed, and reach the signers only through
        # their environment (R2-3): never as an argument of any process.
        for variable in ("target_key", "root_keys", "legacy_minisign_key"):
            self.assertNotRegex(body, r"(?m)^\s*(cat|cp|mv|echo|printf|tee)\b[^\n]*\$\{?" + variable)
        for needle in ("--signing-key ", "--key ", "--target-key)", "--root-key)", "minisign -S -s \"$legacy"):
            self.assertNotIn(needle, body)
        self.assertIn('DUDUCLAW_ISOLATION_TARGET_KEY="$target_key" python3 -I -B scripts/package-linux-release.py',
                      body)
        self.assertIn('DUDUCLAW_ISOLATION_SIGNING_KEY_1="${root_keys[0]}" DUDUCLAW_ISOLATION_SIGNING_KEY_2='
                      '"${root_keys[1]}"', body)
        self.assertIn("--signing-key-from-env", body)
        minisign = shell_function("isolation_minisign_sign")
        self.assertIn('minisign -S -s "$link_dir/signing-key"', minisign)
        self.assertIn('rm -rf -- "$link_dir"', minisign)
        self.assertIn('ISOLATION_LINK_TARGET="$1" python3 -I -B -c', minisign)
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

    # ---------------------------------------------------------------- R2-1: what the ceremony executes

    def test_every_ceremony_python_runs_isolated_without_bytecode(self):
        section = isolation_section()
        code = [line for line in section.replace(shell_function("isolation_sign_usage"), "").splitlines()
                if not line.lstrip().startswith("#")]
        invocations = [match for line in code for match in re.findall(r"python3(?=\s).*", line)]
        self.assertGreaterEqual(len(invocations), 8)
        for invocation in invocations:
            self.assertTrue(invocation.startswith("python3 -I -B "), invocation)
        # Children started by the packager run the same way.
        packager = (ROOT / "scripts/package-linux-release.py").read_text()
        self.assertIn('[sys.executable, "-I", "-B", str(ROOT / "scripts/sign-isolation-manifest.py")', packager)
        self.assertEqual(packager.count('sys.executable, "-I", "-B", str(ROOT / "scripts/check_glibc.py")'), 2)
        self.assertNotRegex(packager, r'sys\.executable, str\(ROOT')
        # Every script release.sh executes or reads by path is compared with the tag's blob.
        files = re.search(r"ISOLATION_CEREMONY_FILES=\(\n(.*?)\n\)", section, re.S)[1].split()
        for path in ("scripts/release.sh", "scripts/package-linux-release.py", "scripts/sign-isolation-manifest.py",
                     "scripts/check_glibc.py", "release/isolation/manifest-template.json",
                     "release/isolation/probe-image.digest", "release/bootstrap-root.sha256",
                     "crates/duduclaw-isolation/src/update.rs", "Cargo.toml"):
            self.assertIn(path, files)
        for path in re.findall(r"python3 -I -B (scripts/[\w.-]+\.py)", section):
            self.assertIn(path, files)
        # The checkout check precedes the first Python step of the ceremony.
        body = isolation_function()
        self.assertLess(body.index('isolation_check_checkout "$tag" || return 1'), body.index("python3"))
        self.assertNotIn("--untracked-files=no", section)

    def make_tag_checkout(self, directory):
        """A throwaway repository whose HEAD is tag v9.9.9, isolated from any user/system git config."""
        repo = Path(directory).resolve() / "repo"
        repo.mkdir()
        env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")

        def git(*args):
            subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True)
        git("init", "-q")
        for key, value in (("user.name", "ceremony test"), ("user.email", "ceremony@example.invalid"),
                           ("commit.gpgsign", "false"), ("tag.gpgsign", "false")):
            git("config", key, value)
        (repo / "scripts").mkdir()
        (repo / "release").mkdir()
        (repo / "scripts/sign-isolation-manifest.py").write_text("import hashlib\n")
        (repo / "scripts/package-linux-release.py").write_text("import subprocess\n")
        (repo / "release/manifest-template.json").write_text("{}\n")
        (repo / ".gitignore").write_text("__pycache__/\n*.pyc\ntarget/\n")
        git("add", ".")
        git("commit", "-qm", "ceremony fixture")
        git("tag", "v9.9.9")
        return repo, env, git

    def check_checkout(self, repo, env):
        files = "scripts/sign-isolation-manifest.py scripts/package-linux-release.py release/manifest-template.json"
        return run_functions(["isolation_fail", "isolation_check_checkout"],
                             f"ISOLATION_CEREMONY_FILES=({files})\nisolation_check_checkout v9.9.9\n",
                             cwd=repo, env=env)

    def test_checkout_must_be_the_tag_without_untracked_ignored_flagged_or_modified_files(self):
        with tempfile.TemporaryDirectory() as directory:
            repo, env, git = self.make_tag_checkout(directory)
            result = self.check_checkout(repo, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            # Ignored build output outside scripts/ and release/ cannot be imported under -I.
            (repo / "target").mkdir()
            (repo / "target/build.o").write_text("object")
            result = self.check_checkout(repo, env)
            self.assertEqual(result.returncode, 0, result.stderr)

            # An untracked .py next to the scripts would shadow the standard library.
            for name in ("scripts/hashlib.py", "scripts/subprocess.py", "notes.txt"):
                with self.subTest(untracked=name):
                    (repo / name).write_text("raise SystemExit('planted')\n")
                    result = self.check_checkout(repo, env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("untracked files", result.stderr)
                    (repo / name).unlink()

            # Ignored files where the ceremony loads code (bytecode caches included).
            cache = repo / "scripts/__pycache__"
            cache.mkdir()
            (cache / "sign-isolation-manifest.cpython-312.pyc").write_bytes(b"planted")
            result = self.check_checkout(repo, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("ignored files exist under scripts/ or release/", result.stderr)
            (cache / "sign-isolation-manifest.cpython-312.pyc").unlink()
            cache.rmdir()

            # A modified tracked script.
            script = repo / "scripts/sign-isolation-manifest.py"
            script.write_text("import hashlib\nprint('modified')\n")
            result = self.check_checkout(repo, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("uncommitted changes", result.stderr)

            # The same edit hidden from git status by index flags.
            for flag in ("--assume-unchanged", "--skip-worktree"):
                with self.subTest(flag=flag):
                    git("update-index", flag, "scripts/sign-isolation-manifest.py")
                    status = subprocess.run(["git", "status", "--porcelain"], cwd=repo, env=env,
                                            capture_output=True, text=True, check=True)
                    self.assertEqual(status.stdout, "")  # git status alone would pass it
                    result = self.check_checkout(repo, env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("index flags", result.stderr)
                    git("update-index", flag.replace("--", "--no-", 1), "scripts/sign-isolation-manifest.py")
            git("checkout", "--", "scripts/sign-isolation-manifest.py")
            self.assertEqual(self.check_checkout(repo, env).returncode, 0)

            # HEAD must be the tag itself.
            (repo / "release/manifest-template.json").write_text('{"changed":1}\n')
            git("commit", "-qam", "after the tag")
            result = self.check_checkout(repo, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("check out tag v9.9.9 first", result.stderr)
            git("checkout", "-q", "v9.9.9")
            self.assertEqual(self.check_checkout(repo, env).returncode, 0)
            # Every listed ceremony file must exist and be tracked at the tag.
            result = run_functions(["isolation_fail", "isolation_check_checkout"],
                                   "ISOLATION_CEREMONY_FILES=(scripts/absent.py)\nisolation_check_checkout v9.9.9\n",
                                   cwd=repo, env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("scripts/absent.py is missing", result.stderr)

    # ---------------------------------------------------------------- R2-2: the docker probe

    def docker_argv(self, binary, image):
        return run_functions(["isolation_fail", "isolation_probe_docker_argv"],
                             f'isolation_probe_docker_argv "{binary}" linux/arm64 "{image}"\n'
                             'printf "%s\\n" "${ISOLATION_DOCKER_ARGV[@]}"\n')

    def test_docker_probe_argv_is_hardened_and_digest_only(self):
        binary = "/tmp/duduclaw-isolation-sign.X/aarch64/probe/duduclaw-bootstrap"
        result = self.docker_argv(binary, DIGEST)
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = result.stdout.splitlines()
        self.assertEqual(argv[:2], ["docker", "run"])
        for flag in ("--rm", "--pull=never", "--read-only", "--cap-drop=ALL"):
            self.assertIn(flag, argv)
        for flag, value in (("--platform", "linux/arm64"), ("--network", "none"), ("--user", "65534:65534"),
                            ("--security-opt", "no-new-privileges"), ("--pids-limit", "32"),
                            ("--memory", "256m"), ("--entrypoint", "/probe/duduclaw-bootstrap")):
            self.assertEqual(argv[argv.index(flag) + 1], value, flag)
        self.assertEqual(argv.count("--mount"), 1)
        self.assertEqual(argv[argv.index("--mount") + 1],
                         f"type=bind,source={binary},target=/probe/duduclaw-bootstrap,readonly")
        self.assertEqual(argv[-2:], [DIGEST, "--print-root-pin"])
        for forbidden in ("-v", "--volume", "-e", "--env", "--env-file", "--privileged", "--cap-add", "-it"):
            self.assertNotIn(forbidden, argv)
        self.assertFalse(any(part.startswith("ubuntu:") for part in argv))
        # Tag-only, tag+digest-less or malformed references, and unsafe mount paths, are refused.
        for image in ("ubuntu:22.04", "ubuntu", "ubuntu@sha256:" + "ab" * 31, "ubuntu:22.04@sha256:" + "ab" * 32,
                      "debian@sha256:" + "ab" * 32, "ubuntu@sha256:" + "AB" * 32):
            with self.subTest(image=image):
                self.assertNotEqual(self.docker_argv(binary, image).returncode, 0)
        for path in ("relative/duduclaw-bootstrap", "/tmp/a,b/duduclaw-bootstrap"):
            self.assertNotEqual(self.docker_argv(path, DIGEST).returncode, 0)
        # The probe only ever runs this argv; the native path is unchanged.
        probe = shell_function("isolation_probe_pin")
        self.assertIn('"${ISOLATION_DOCKER_ARGV[@]}"', probe)
        self.assertIn('env -i "$binary" --print-root-pin', probe)
        self.assertEqual(probe.count("docker run"), 0)
        self.assertNotIn("ubuntu:22.04", isolation_section())

    def test_probe_image_comes_only_from_a_verified_digest_line(self):
        def image(content, platform="linux/amd64"):
            with tempfile.TemporaryDirectory() as directory:
                digest_file = Path(directory) / "probe-image.digest"
                if content is not None:
                    digest_file.write_text(content)
                return run_functions(["isolation_fail", "isolation_probe_image"],
                                     f'ISOLATION_PROBE_DIGEST_FILE="{digest_file}"\n'
                                     f'isolation_probe_image {platform}\n')
        result = image(f"# comment\nlinux/amd64 {DIGEST}\nlinux/arm64 PLACEHOLDER\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, DIGEST + "\n")
        for content in (f"linux/amd64 {DIGEST}\nlinux/amd64 {DIGEST}\n", "linux/amd64 ubuntu:22.04\n",
                        "linux/amd64 PLACEHOLDER-operator-must-replace\n", f"linux/arm64 {DIGEST}\n", "", None,
                        f"linux/amd64 {DIGEST} extra\n"):
            with self.subTest(content=content):
                result = image(content)
                self.assertNotEqual(result.returncode, 0)
        # The tracked file ships with placeholders only: the docker path refuses until an
        # operator fills in digests they verified (never invented here).
        tracked = (ROOT / "release/isolation/probe-image.digest").read_text()
        self.assertNotRegex(tracked, r"sha256:[0-9a-f]{64}")
        for platform in ("linux/amd64", "linux/arm64"):
            result = run_functions(["isolation_fail", "isolation_probe_image"],
                                   'ISOLATION_PROBE_DIGEST_FILE="release/isolation/probe-image.digest"\n'
                                   f"isolation_probe_image {platform}\n")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no verified digest", result.stderr)

    # ---------------------------------------------------------------- R2-3: key locations

    def test_key_locations_come_from_the_environment_never_argv(self):
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / "duduclaw-target-handle"
            key.write_text("handle")
            env = dict(os.environ, DUDUCLAW_ISOLATION_TARGET_KEY=str(key))
            result = run_functions(["isolation_fail", "isolation_key_location"],
                                   'isolation_key_location DUDUCLAW_ISOLATION_TARGET_KEY "target key handle"\n', env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, f"{key}\n")
            env["DUDUCLAW_ISOLATION_TARGET_KEY"] = str(key) + ".missing"
            result = run_functions(["isolation_fail", "isolation_key_location"],
                                   'isolation_key_location DUDUCLAW_ISOLATION_TARGET_KEY "target key handle"\n', env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn(str(key), result.stderr)  # messages name the variable, never the location
            self.assertIn("DUDUCLAW_ISOLATION_TARGET_KEY", result.stderr)


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
