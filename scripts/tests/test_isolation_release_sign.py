"""release.sh isolation-sign, its launcher scripts/isolation-sign and scripts/isolation-test-keys.sh
fail closed (plan R2 §4.2).

Nothing here touches a real key, the network or GitHub: every case stops at a
refusal before the first download or signature (or at --preflight). The
end-to-end cases run the real two-stage ceremony against a throwaway repository
whose HEAD is a tag, with fake tools first in PATH that would leave a marker file.
Normal ceremony starts execute the real launcher. Hostile stage-1 probes deliberately
start release.sh with the launcher's environment plus the forbidden variables;
otherwise the launcher would remove the very condition those probes must exercise.
"""
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RELEASE = ROOT / "scripts/release.sh"
LAUNCHER = ROOT / "scripts/isolation-sign"
LAUNCHER_SHEBANG = "#!/usr/bin/env -S -i /bin/bash --noprofile --norc"
TEST_KEYS = ROOT / "scripts/isolation-test-keys.sh"
PIN_FILE = ROOT / "release/bootstrap-root.sha256"
SIGNER = ROOT / "scripts/sign-isolation-manifest.py"
SPEC = importlib.util.spec_from_file_location("linux_release", ROOT / "scripts/package-linux-release.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)
META_SPEC = importlib.util.spec_from_file_location("isolation_root_metadata", ROOT / "scripts/isolation-root-metadata.py")
META = importlib.util.module_from_spec(META_SPEC)
META_SPEC.loader.exec_module(META)
VERSION = PACKAGE.version_text(PACKAGE.workspace_version())

SYSTEM_DIRS = ("/usr/bin", "/bin", "/usr/sbin", "/sbin")
TOOL_DIRS = SYSTEM_DIRS + ("/opt/homebrew/bin", "/usr/local/bin")
KEY_VARIABLE = re.compile(r"DUDUCLAW_ISOLATION_\w*KEY\w*")
# Fakes put first in PATH: any of them running leaves a line in the marker file.
FAKE_TOOLS = ("git", "python3", "python", "env", "mktemp", "mkdir", "rm", "chmod", "cp", "date", "uname", "gh",
              "minisign", "ssh-keygen", "docker", "cat", "sed", "grep", "cmp", "basename", "readlink", "stat",
              "xcrun", "tar")


def system_tool(name, directories=SYSTEM_DIRS):
    for directory in directories:
        path = os.path.join(directory, name)
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None


def system_python_works():
    python = system_tool("python3")
    if python is None:
        return False
    try:
        result = subprocess.run([python, "-I", "-B", "-c", "import sys; sys.exit(sys.version_info < (3, 9))"],
                                env={"PATH": ":".join(SYSTEM_DIRS)}, capture_output=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return False
    return result.returncode == 0


BASH = "/bin/bash" if os.path.isfile("/bin/bash") else (system_tool("bash") or "bash")
GIT = system_tool("git") or shutil.which("git") or "git"
SSH_KEYGEN = system_tool("ssh-keygen", TOOL_DIRS)


def ceremony_candidates():
    """The candidates release.sh resolves, in its order (None when absent)."""
    candidates = {name: system_tool(name) for name in ("env", "bash", "git", "python3", "mktemp", "mkdir", "rm",
                                                        "chmod", "cp", "date", "uname")}
    candidates.update({name: system_tool(name, TOOL_DIRS) for name in ("gh", "minisign", "docker")})
    candidates["ssh-keygen"] = system_tool("ssh-keygen", TOOL_DIRS[4:] + SYSTEM_DIRS)
    return candidates


def ceremony_tools_pass_audit():
    """Every required tool exists and every present tool passes release.sh's own location audit
    (a group-writable Homebrew, for example, is refused by design: then these cases skip)."""
    candidates = ceremony_candidates()
    if any(candidates[name] is None for name in candidates if name not in ("gh", "minisign", "docker")):
        return False
    if not system_python_works():
        return False
    code = re.search(r"ISOLATION_TOOL_AUDIT_PY='(.*?)'\n", RELEASE.read_text(), re.S)[1]
    result = subprocess.run([sys.executable, "-I", "-B", "-c", code,
                             *(f"X={path}" for path in candidates.values() if path is not None)],
                            capture_output=True, timeout=60)
    return result.returncode == 0


CEREMONY_TOOLS = ceremony_tools_pass_audit()
OPTIONAL_TOOLS = all(ceremony_candidates()[name] for name in ("gh", "minisign", "docker"))


def launcher_works():
    """The real launcher can run here: /usr/bin/env -S -i empties the environment, /bin/bash and
    /usr/bin/python3 work, and this non-root user's home directory exists and is theirs."""
    if os.geteuid() == 0 or not os.path.isfile("/bin/bash") or not os.access("/usr/bin/python3", os.X_OK):
        return False
    try:
        home = os.stat(pwd.getpwuid(os.getuid()).pw_dir)
        emptied = subprocess.run(["/usr/bin/env", "-S", "-i /usr/bin/env"], env={"PROBE": "1"},
                                 capture_output=True, text=True, timeout=30)
        python = subprocess.run(["/usr/bin/python3", "-I", "-S", "-B", "-c", "import pwd"], env={},
                                capture_output=True, timeout=60)
    except (OSError, KeyError, subprocess.SubprocessError):
        return False
    return (stat.S_ISDIR(home.st_mode) and home.st_uid == os.getuid() and emptied.returncode == 0
            and "PROBE" not in emptied.stdout and python.returncode == 0)


LAUNCHER_WORKS = launcher_works()


def launcher_environment(cwd, **extra):
    """Exactly what scripts/isolation-sign exports before it starts release.sh (marker = the
    repository it found, which is the physical working directory)."""
    environment = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": pwd.getpwuid(os.getuid()).pw_dir,
                   "TERM": "dumb", "DUDUCLAW_ISOLATION_LAUNCHER": os.path.realpath(cwd)}
    environment.update(extra)
    return environment


def launcher_layout(base):
    """<base>/repo with the launcher (0755), release.sh and Cargo.toml: enough for stage 1's
    tool-free refusals (no production pin in it)."""
    repo = base / "repo"
    (repo / "scripts").mkdir(parents=True)
    shutil.copyfile(LAUNCHER, repo / "scripts/isolation-sign")
    (repo / "scripts/isolation-sign").chmod(0o755)
    shutil.copyfile(RELEASE, repo / "scripts/release.sh")
    shutil.copyfile(ROOT / "Cargo.toml", repo / "Cargo.toml")
    return repo


def hostile_environment(base, **extra):
    """A polluted caller: exported functions named hash, declare, git, /usr/bin/env, compgen and pwd,
    a BASH_ENV file and key-location variables. Any of them running leaves a line in the marker."""
    marker = base / "hostile-marker"
    bash_env = base / "bash-env.sh"
    bash_env.write_text(f'echo "BASH_ENV ran" >> "{marker}"\n')
    environment = clean_environment(BASH_ENV=str(bash_env), DUDUCLAW_ISOLATION_TARGET_KEY="/keys/target",
                                    DUDUCLAW_ISOLATION_ROOT_KEY_1="/keys/r1", SSH_AUTH_SOCK=str(base / "agent.sock"),
                                    GIT_DIR=str(base / "nowhere"), PYTHONPATH=str(base / "fakes"), **extra)
    for name in ("hash", "declare", "git", "/usr/bin/env", "compgen", "pwd"):
        environment[f"BASH_FUNC_{name}%%"] = f'() {{ echo "function {name} ran" >> "{marker}"; }}'
    return environment, marker


def ceremony_leftovers():
    """Snapshot / work directories in the places the real launcher's stage 1 may use."""
    bases = {"/tmp"}
    if sys.platform == "darwin":
        result = subprocess.run(["/usr/bin/getconf", "DARWIN_USER_TEMP_DIR"], capture_output=True, text=True)
        if result.returncode == 0 and result.stdout.strip():
            bases.add(result.stdout.strip())
    found = set()
    for base in bases:
        try:
            names = os.listdir(base)
        except OSError:
            continue
        found |= {os.path.join(os.path.realpath(base), name) for name in names
                  if name.startswith(("duduclaw-isolation-ceremony.", "duduclaw-isolation-sign."))}
    return found


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


def section_code():
    """The isolation section without the help text and without comment lines."""
    section = isolation_section().replace(shell_function("isolation_sign_usage"), "")
    return "\n".join(line for line in section.splitlines() if not line.lstrip().startswith("#"))


def python_constant(name):
    """A single-quoted python3 program assigned in release.sh (NAME='...')."""
    return re.search(name + r"='(.*?)'\n", release_text(), re.S)[1]


def ceremony_files():
    return re.search(r"ISOLATION_CEREMONY_FILES=\(\n(.*?)\n\)", isolation_section(), re.S)[1].split()


def run_functions(names, command, cwd=ROOT, env=None):
    script = "set -euo pipefail\n" + "".join(shell_function(name) for name in names) + command
    return subprocess.run([BASH, "-c", script], cwd=cwd, env=env, capture_output=True, text=True, timeout=60)


def run_section(command, cwd=ROOT, env=None):
    """The whole isolation section (assignments and functions only) plus `command`."""
    script = "set -euo pipefail\n" + isolation_section() + command
    return subprocess.run([BASH, "-c", script], cwd=cwd, env=env, capture_output=True, text=True, timeout=120)


def clean_environment(**extra):
    environment = {key: value for key, value in os.environ.items()
                   if key not in ("CI", "GITHUB_ACTIONS", "BASH_ENV") and not KEY_VARIABLE.fullmatch(key)
                   and not key.startswith("BASH_FUNC_")}
    environment.update(extra)
    return environment


def write_fakes(directory, marker):
    directory.mkdir(exist_ok=True)
    for name in FAKE_TOOLS:
        path = directory / name
        path.write_text(f'#!/bin/sh\necho "{name} $*" >> "{marker}"\nexit 97\n')
        path.chmod(0o755)
    # A planted module next to nothing: only an interpreter without -I would import it.
    (directory / "hashlib.py").write_text(f'open("{marker}", "a").write("hashlib planted\\n")\n')


def make_snapshot(source, snapshot, files):
    """A snapshot laid out like stage 1 writes it (files 0400, directories 0500)."""
    snapshot.mkdir(mode=0o700)
    for path in files:
        target = snapshot / path
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / path, target)
    for directory, subdirectories, names in os.walk(snapshot, topdown=False):
        for name in names:
            os.chmod(os.path.join(directory, name), 0o400)
        os.chmod(directory, 0o500)
    return snapshot


def make_writable(tree):
    for directory, _, names in os.walk(tree):
        os.chmod(directory, 0o700)
        for name in names:
            os.chmod(os.path.join(directory, name), 0o600)


DIGEST = "ubuntu@sha256:" + "ab" * 32


class IsolationSignTests(unittest.TestCase):
    def run_sign(self, *args, env=None, stdin=subprocess.DEVNULL):
        """Use the real launcher; only hostile stage-1 environment probes bypass it deliberately."""
        if env is None:
            environment = clean_environment()
            command = [str(LAUNCHER), *args]
        else:
            environment = launcher_environment(ROOT, **env)
            command = [BASH, "--noprofile", "--norc", str(RELEASE), "isolation-sign", *args]
        return subprocess.run(command, cwd=ROOT,
                              env=environment, stdin=stdin, capture_output=True, text=True, timeout=60)

    def required(self):
        # Key locations are never arguments nor environment variables (R2-3): only the prompt.
        arguments = ["--root-metadata", "root.json",
                     "--root-signatures", "root.json.signatures.json", "--output-dir", "/nonexistent/out"]
        if VERSION in release_text().split('ISOLATION_MIGRATION_BUNDLE_VERSIONS="', 1)[1].split('"', 1)[0].split():
            arguments += ["--migration-bundle"]
        return arguments

    def test_help_documents_every_step_both_stages_and_the_secret_rules(self):
        result = self.run_sign("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        for needle in ("PRODUCTION <pin>", "Q6", "ONLY v1.83.0", "--migration-bundle", "BundleAuthorization",
                       "SHA256SUMS-isolation.txt", "type UPLOAD", "without replacing any asset",
                       "never copies, prints or logs a key", "[external] gate", "Never in CI",
                       "STAGE 1", "STAGE 2", "handles NO secret", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                       "/opt/homebrew/bin and /usr/local/bin", "NOTICE: trusting <directory>",
                       "FROM THE TAG'S BLOB", "Stage 1: snapshot of v<version>", "env -i", "no SSH_AUTH_SOCK",
                       "removed on", "--preflight", "ONLY on the helper's stdin", "R1 then R2",
                       "ssh-keygen -Y verify", "NEVER environment variables", "DUDUCLAW_ISOLATION_*KEY*",
                       "Usage: scripts/isolation-sign", LAUNCHER_SHEBANG, "never through sudo",
                       "never as bash scripts/release.sh isolation-sign", "as a CHILD process", "ssh-sk-helper",
                       "SSH_SK_HELPER", "copied ONCE", "target_key of the pinned root",
                       "--output-dir is NOT where the ceremony works", "COPIED into --output-dir",
                       "private staging directory", "neither a symbolic link nor group/other-writable",
                       "immediately before the", "A failed copy never affects what was uploaded"):
            self.assertIn(needle, result.stdout)
        usage = subprocess.run([BASH, str(RELEASE)], cwd=ROOT, capture_output=True, text=True)
        self.assertIn("scripts/isolation-sign --help", usage.stdout)

    def test_refused_in_ci_before_anything_else(self):
        # CI variables never reach a launcher start (the launcher empties the environment); a start
        # that still carries them is not the launcher's and is refused before anything else.
        for variable in ("CI", "GITHUB_ACTIONS"):
            result = self.run_sign(*self.required(), env={variable: "true"})
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("runs only when started as scripts/isolation-sign", result.stderr)
            self.assertIn(f"unexpected environment variable(s): {variable}", result.stderr)
        # Stage 2 keeps its own CI refusal.
        self.assertIn("never runs in CI", shell_function("isolation_refuse_hostile_start"))

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
        joined = re.sub(r"\\\n\s*", "", body)
        joined_branch = re.sub(r"\\\n\s*", "", branch)
        for needle in ("--migration-bundle-from", "--kind bundle --bundle", "--kind bundle --check-threshold",
                       "--minisign-sign"):
            self.assertEqual(joined.count(needle), 1, needle)
            self.assertIn(needle, joined_branch)
        self.assertNotIn("minisign -S", body)
        # R2-5: the embedded-key threshold check runs (and deletes on failure) before
        # anything of the bundle is added to the upload list.
        check = joined_branch.index("--kind bundle --check-threshold")
        self.assertLess(check, joined_branch.index('-f -- "$authorization" "$authorization.signatures.json"'))
        self.assertLess(check, joined_branch.index("uploads+="))

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

    def test_no_secret_is_copied_printed_exported_or_passed_as_argument(self):
        body = isolation_function()
        section = isolation_section()
        self.assertNotRegex(section, r"(?m)^\s*set -x")
        # Key locations are never copied, printed or exported, and reach the signers only on
        # their stdin through isolation_handoff (R2-3): never argv, never the environment.
        for variable in ("target_key", "root_keys", "legacy_minisign_key"):
            self.assertNotRegex(body, r"(?m)^\s*(cat|cp|mv|echo|printf|tee)\b[^\n]*\$\{?" + variable)
            self.assertNotRegex(section, r"(export|declare -x|declare -gx)[^\n]*" + variable)
            uses = re.findall(r"[^\n]*\$\{?" + variable + r"\b[^\n]*", body)
            for use in uses:
                self.assertTrue(use.lstrip().startswith(("isolation_handoff ", 'if [[ "${root_keys[0]}"', variable,
                                                         "root_keys+=")), use)
        for needle in ("--signing-key ", "--key ", "--target-key)", "--root-key)", "minisign -S",
                       "DUDUCLAW_ISOLATION_", "signing-key-from-env"):
            self.assertNotIn(needle, body)
        joined = re.sub(r"\\\n\s*", "", body)
        self.assertIn('isolation_handoff 1 "$target_key" "$python" -I -B "$snapshot/scripts/package-linux-release.py"',
                      joined)
        self.assertIn('isolation_handoff 2 "${root_keys[0]}" "${root_keys[1]}" "$python" -I -B '
                      '"$snapshot/scripts/sign-isolation-manifest.py" --kind bundle', joined)
        self.assertIn('isolation_handoff 1 "$legacy_minisign_key" "$python" -I -B '
                      '"$snapshot/scripts/sign-isolation-manifest.py" --minisign-sign', joined)
        self.assertIn("--signing-key-stdin", body)
        handoff = shell_function("isolation_handoff")
        self.assertIn("printf '%s\\n' \"${locations[@]}\" | isolation_run \"$@\"", handoff)
        prompt = shell_function("isolation_key_location")
        self.assertNotIn("${!", prompt)  # no variable is ever read as a location
        stage2 = shell_function("isolation_stage2")
        self.assertIn('ISOLATION_WORK="$(isolation_run "$ISOLATION_TOOL_MKTEMP" -d', stage2)
        self.assertIn("trap isolation_cleanup EXIT", shell_function("isolation_set_traps"))
        self.assertIn('if [[ ! -t 0 ]]; then', body)
        self.assertNotIn("--clobber", body)
        self.assertIn('isolation_run "$ISOLATION_TOOL_GH" release upload "$tag" --repo "$ISOLATION_REPO" '
                      '"${uploads[@]}"', body)
        self.assertIn('if [[ "$answer" != "UPLOAD" ]]; then', body)
        # Re-hash before upload and read every asset back afterwards.
        self.assertLess(body.index("changed after it was recorded"), body.index("release upload"))
        self.assertLess(body.index("release upload"), body.index("differs from the signed bytes"))
        # The released verifier must print exactly PRODUCTION <pin> (layer 2), and
        # the published sums are computed from the bytes uploaded (layer 3).
        self.assertIn('if [[ "$probe_output" != "PRODUCTION $pin" ]]; then', body)
        self.assertIn('hashes+=("$(isolation_sha256 "${uploads[$index]}")")', body)
        self.assertIn("--from-release-archive", body)
        self.assertNotIn("--test-signer", body)
        self.assertNotIn("isolation-test-keys", body)
        # Preflight stops before the first prompt.
        self.assertLess(body.index('if [[ "$ISOLATION_PREFLIGHT" == "1" ]]; then'),
                        body.index('isolation_key_location "target key handle"'))

    def test_dry_run_line_points_to_the_offline_step(self):
        text = release_text()
        self.assertNotIn("F metadata is signed offline only", text)
        self.assertIn("isolation-sign --help (owner present; migration bundle only for:", text)

    # ---------------------------------------------------------------- R2-1: what the ceremony executes

    def test_path_is_fixed_first_and_isolation_sign_is_dispatched_before_anything_else(self):
        text = release_text()
        guard = text.index('if [[ "${1:-}" == "isolation-sign" ]]; then\n    PATH="/usr/bin:/bin:/usr/sbin:/sbin"')
        self.assertLess(text.index("set -euo pipefail"), guard)
        before = [line for line in text[:guard].splitlines() if line.strip() and not line.lstrip().startswith("#")]
        self.assertEqual(before, ["set -euo pipefail"])
        self.assertLess(guard, text.index("\nisolation_fail() {"))
        # No pre-check before the launcher check (a hostile shell could replace `hash` or `declare`):
        # the top only fixes PATH, and stage 1's first statement is the launcher check.
        top = text[guard:text.index("\nfi\n", guard)]
        self.assertEqual([line.strip() for line in top.splitlines()[1:]],
                         ['PATH="/usr/bin:/bin:/usr/sbin:/sbin"', "export PATH"])
        self.assertNotIn("ISOLATION_IMPORTED_FUNCTIONS", text)
        stage1 = shell_function("isolation_stage1").splitlines()
        self.assertEqual(stage1[1].strip(), "isolation_require_launcher || return 1")
        self.assertEqual(stage1[2].strip(), "isolation_refuse_hostile_start || return 1")
        check = shell_function("isolation_require_launcher")
        self.assertIn("$(compgen -e)", check)
        self.assertIn("not against a hostile shell", text[text.index("# Stage 1's FIRST check"):])
        dispatch = text.index('if [[ "${1:-}" == "isolation-sign" ]]; then\n    shift')
        self.assertLess(dispatch, text.index('CURRENT_VERSION="$(extract_version'))
        self.assertNotIn("run_isolation_sign \"$@\"", text)
        self.assertIn('isolation_stage2 "$@"', text[dispatch:dispatch + 400])
        self.assertIn('isolation_stage1 "$@"', text[dispatch:dispatch + 400])

    def test_no_bare_command_and_every_python_runs_isolated_from_the_snapshot(self):
        code = section_code()
        tools = sorted(set(FAKE_TOOLS) | {"awk", "tr", "head", "realpath", "tee"}, key=len, reverse=True)
        bare = re.compile(r"(?:^|[;&|]|\$\(|\bthen\b|\bdo\b|\belse\b)\s*(" + "|".join(map(re.escape, tools))
                          + r")(?=\s|$)", re.M)
        self.assertEqual(bare.findall(code), [])
        # Every interpreter start: absolute path, -I -B, allow-listed environment.
        starts = re.findall(r'"\$(?:python|ISOLATION_TOOL_PYTHON|python_path)" (\S+) (\S+)', code)
        invocations = re.findall(r'(?:isolation_run|isolation_handoff [^\n]*?)\s*"\$(?:python|ISOLATION_TOOL_PYTHON)"'
                                 r' -I -B', code)
        self.assertGreaterEqual(len(invocations), 12)
        for flags in starts:
            if flags[0].startswith(("ssh_keygen=", "\"")) or flags == ("-I", "-B"):
                continue
            self.fail(f"python started without -I -B: {flags}")
        self.assertIn('"$env_path" -i "PATH=$ISOLATION_SYSTEM_PATH" "$python_path" -I -B -c', code)
        self.assertNotRegex(code, r"(?m)(^|[|;(]|\$\()\s*python3\s")
        # Python helpers only from the snapshot.
        for line in code.splitlines():
            for match in re.finditer(r'(\S*)scripts/([\w.-]+\.py)', line):
                if "-I -B" in line or "--" in line:
                    self.assertTrue(match[1].endswith('"$snapshot/'), line)
        # Children started by the packager run the same way.
        packager = (ROOT / "scripts/package-linux-release.py").read_text()
        self.assertIn('[sys.executable, "-I", "-B", str(ROOT / "scripts/sign-isolation-manifest.py")', packager)
        self.assertEqual(packager.count('sys.executable, "-I", "-B", str(ROOT / "scripts/check_glibc.py")'), 2)
        self.assertNotRegex(packager, r'sys\.executable, str\(ROOT')
        self.assertIn("env=signer.child_environment()", packager)
        self.assertNotIn("signing-key-from-env", packager)
        self.assertNotIn("DUDUCLAW_ISOLATION_TARGET_KEY", packager)
        # Every file stage 2 reads or executes is in the snapshot list.
        files = ceremony_files()
        for path in ("scripts/release.sh", "scripts/package-linux-release.py", "scripts/sign-isolation-manifest.py",
                     "scripts/check_glibc.py", "release/isolation/manifest-template.json",
                     "release/isolation/probe-image.digest", "release/bootstrap-root.sha256",
                     "crates/duduclaw-isolation/src/update.rs", "Cargo.toml"):
            self.assertIn(path, files)
        for path in (ROOT / "release/isolation").iterdir():
            self.assertIn(str(path.relative_to(ROOT)), files)
        for path in re.findall(r'"\$snapshot/(scripts/[\w.-]+\.py)"', code):
            self.assertIn(path, files)
        # The repository gate precedes the first Python step of the ceremony body.
        body = isolation_function()
        self.assertLess(body.index('isolation_check_repository "$tag" || return 1'), body.index('isolation_run "$python"'))

    def test_git_runs_only_scrubbed_through_one_wrapper(self):
        code = section_code()
        self.assertEqual(code.count('"$ISOLATION_TOOL_GIT"'), 1)
        self.assertIn('"$ISOLATION_TOOL_ENV" -i "${ISOLATION_GIT_ENV[@]}" "$ISOLATION_TOOL_GIT" '
                      '"${ISOLATION_GIT_OPTIONS[@]}" "$@"', shell_function("isolation_git"))
        options = re.search(r"ISOLATION_GIT_OPTIONS=\((.*?)\)\n", isolation_section(), re.S)[1].split()
        for option in ("core.fsmonitor=", "core.hooksPath=/dev/null", "core.pager=cat", "protocol.allow=never",
                       "core.attributesFile=/dev/null", "credential.helper=", "core.askPass="):
            self.assertIn(option, options)
        self.assertIn("--no-replace-objects", options)
        environment = re.search(r"ISOLATION_GIT_ENV=\((.*?)\)\n", isolation_section(), re.S)[1].split()
        for setting in ("GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=/dev/null", "GIT_TERMINAL_PROMPT=0",
                        "GIT_OPTIONAL_LOCKS=0"):
            self.assertIn(setting, environment)
        self.assertIn('for name in ${!GIT_@}; do unset "$name"; done', shell_function("isolation_scrub_git_environment"))
        # Only commands that never run a filter, hook or textconv.
        commands = set(re.findall(r"isolation_git (\S+)", code))
        self.assertEqual(commands - {"rev-parse", "ls-tree", "cat-file", "hash-object", '"${ISOLATION_GIT_ENV[@]}"'},
                         set())
        self.assertIn("hash-object --no-filters", code)
        self.assertNotIn("isolation_git status", code)

    # ---------------------------------------------------------------- tool resolution

    def audit(self, *candidates):
        return subprocess.run([sys.executable, "-I", "-B", "-c", python_constant("ISOLATION_TOOL_AUDIT_PY"),
                               *candidates], capture_output=True, text=True, timeout=60)

    def test_tool_audit_rule_and_user_owned_notice(self):
        env = system_tool("env")
        result = self.audit(f"ENV={env}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"ENV\t{os.path.realpath(env)}\n")
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve() / "bin"
            directory.mkdir(mode=0o755)
            tool = directory / "gh"
            tool.write_text("#!/bin/sh\n")
            tool.chmod(0o755)
            result = self.audit(f"GH={tool}")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(f"NOTICE: trusting {directory}: this tool directory is owned by your user", result.stderr)
            self.assertEqual(result.stderr.count("NOTICE"), 1)
            # Group/other-writable tool, group/other-writable directory, not executable,
            # relative path, link into a writable directory: all refused.
            tool.chmod(0o775)
            self.assertNotEqual(self.audit(f"GH={tool}").returncode, 0)
            tool.chmod(0o755)
            directory.chmod(0o777)
            self.assertNotEqual(self.audit(f"GH={tool}").returncode, 0)
            directory.chmod(0o755)
            tool.chmod(0o644)
            self.assertNotEqual(self.audit(f"GH={tool}").returncode, 0)
            tool.chmod(0o755)
            self.assertNotEqual(self.audit("GH=bin/gh").returncode, 0)
            writable = Path(temporary).resolve() / "writable"
            writable.mkdir(mode=0o777)
            writable.chmod(0o777)
            (writable / "real-gh").write_text("#!/bin/sh\n")
            (writable / "real-gh").chmod(0o755)
            link = directory / "minisign"
            link.symlink_to(writable / "real-gh")
            result = self.audit(f"MINISIGN={link}")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing", result.stderr)

    @unittest.skipUnless(CEREMONY_TOOLS, "needs env, bash, git, python3 (3.9+) ... in system directories")
    def test_tools_resolve_only_from_the_allow_list_never_path(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            marker = base / "marker"
            write_fakes(base / "fakes", marker)
            env = clean_environment(PATH=f"{base / 'fakes'}:/usr/bin:/bin")
            result = run_section('ISOLATION_SSH_KEYGEN_OPTION=""\nisolation_resolve_tools\n'
                                 'for v in ENV BASH GIT PYTHON MKTEMP MKDIR RM CHMOD CP DATE UNAME SSH_KEYGEN; do\n'
                                 '  name="ISOLATION_TOOL_$v"; printf "%s=%s\\n" "$v" "${!name}"\ndone\n', env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            resolved = dict(line.split("=", 1) for line in result.stdout.splitlines())
            self.assertEqual(len(resolved), 12)
            for name, path in resolved.items():
                self.assertTrue(path.startswith("/"), name)
                self.assertNotIn(str(base), path)
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
            # --ssh-keygen must name ssh-keygen inside the allow-listed directories.
            for option in (str(base / "fakes/ssh-keygen"), "/usr/bin/../bin/ssh-keygen", "ssh-keygen",
                           "/usr/bin/python3"):
                with self.subTest(option=option):
                    result = run_section(f'ISOLATION_SSH_KEYGEN_OPTION="{option}"\nisolation_resolve_tools\n', env=env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("--ssh-keygen must be the absolute path of ssh-keygen", result.stderr)
            self.assertFalse(marker.exists())

    # ---------------------------------------------------------------- stage 1 refusals before any tool

    def refused_before_any_tool(self, extra, message, launched=True):
        """Stage 1 refuses with `message` before any tool runs. launched: started with the
        launcher's environment plus `extra`; otherwise started directly from an ordinary shell."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            marker = base / "marker"
            write_fakes(base / "fakes", marker)
            if launched:
                environment = launcher_environment(ROOT, PATH=f"{base / 'fakes'}:/usr/bin:/bin", TMPDIR=str(base),
                                                   **extra)
                command = [BASH, "--noprofile", "--norc", str(RELEASE), "isolation-sign", *self.required()]
            else:
                environment = clean_environment(PATH=f"{base / 'fakes'}:/usr/bin:/bin", TMPDIR=str(base), **extra)
                command = [BASH, str(RELEASE), "isolation-sign", *self.required()]
            controller, terminal = os.openpty()
            try:
                result = subprocess.run(command, cwd=ROOT, env=environment, stdin=terminal, capture_output=True,
                                        text=True, timeout=60)
            finally:
                os.close(controller)
                os.close(terminal)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)
            self.assertNotIn("Stage 1: snapshot", result.stdout)
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
            self.assertEqual(sorted(path.name for path in base.iterdir()), ["fakes"])
            return result

    def test_direct_start_of_release_sh_is_refused_before_any_tool_runs(self):
        # `bash scripts/release.sh isolation-sign` (and --help) from an ordinary shell: no marker.
        result = self.refused_before_any_tool({}, "runs only when started as scripts/isolation-sign", launched=False)
        self.assertIn("never bash scripts/release.sh isolation-sign, never through sudo", result.stderr)
        direct = subprocess.run([BASH, str(RELEASE), "isolation-sign", "--help"], cwd=ROOT, env=clean_environment(),
                                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        self.assertNotEqual(direct.returncode, 0)
        self.assertIn("runs only when started as scripts/isolation-sign", direct.stderr)
        # A forged marker naming another directory is refused as well.
        self.refused_before_any_tool({"DUDUCLAW_ISOLATION_LAUNCHER": "/elsewhere"},
                                     "runs only when started as scripts/isolation-sign")
        # The enterprise build default must not leak into the offline environment;
        # an externally supplied value is still forbidden, not allow-listed.
        self.refused_before_any_tool({"CLOUD_BUILD": "1"},
                                     "unexpected environment variable(s): CLOUD_BUILD")

    def test_key_location_variable_in_the_environment_is_refused_in_stage_1(self):
        for variable in ("DUDUCLAW_ISOLATION_TARGET_KEY", "DUDUCLAW_ISOLATION_ROOT_KEY_2",
                         "DUDUCLAW_ISOLATION_LEGACY_MINISIGN_KEY", "DUDUCLAW_ISOLATION_SIGNING_KEY_1"):
            with self.subTest(variable=variable):
                self.refused_before_any_tool({variable: "/somewhere/key"},
                                             f"unexpected environment variable(s): {variable}")
        # Stage 2 keeps its own refusal of such variables.
        self.assertIn("key locations are entered only at the attended prompt",
                      shell_function("isolation_refuse_hostile_start"))

    def test_exported_functions_and_bash_env_are_refused_in_stage_1(self):
        # Secondary assertion after the launcher check (a launcher start has neither).
        self.refused_before_any_tool({"BASH_FUNC_duduclaw_planted%%": "() { :; }"}, "exports shell functions")
        with tempfile.TemporaryDirectory() as temporary:
            empty = Path(temporary) / "empty.sh"
            empty.write_text("")
            self.refused_before_any_tool({"BASH_ENV": str(empty)}, "unexpected environment variable(s): BASH_ENV")

    # ---------------------------------------------------------------- the launcher

    def test_launcher_is_tracked_executable_with_the_exact_shebang_and_no_environment_logic(self):
        text = LAUNCHER.read_text()
        lines = text.splitlines()
        self.assertEqual(lines[0], LAUNCHER_SHEBANG)
        code = [line.strip() for line in lines[1:] if line.strip() and not line.lstrip().startswith("#")]
        self.assertEqual(code[0], "PATH=/usr/bin:/bin:/usr/sbin:/sbin")
        self.assertEqual(code[-1], 'exec /bin/bash --noprofile --norc "$repository/scripts/release.sh" '
                                   'isolation-sign "$@"')
        joined = "\n".join(code)
        self.assertIn("/usr/bin/python3 -I -S -B -c", joined)
        self.assertIn("pwd.getpwuid(uid).pw_dir", joined)
        self.assertIn("TERM=dumb", joined)
        self.assertIn('DUDUCLAW_ISOLATION_LAUNCHER="$repository"', joined)
        self.assertIn('if [[ -L "$0" ]]; then', joined)
        self.assertIn('"$EUID" == "0"', joined)
        # Nothing is read from the caller's environment: every expanded variable is a bash builtin
        # one, the script's own, or assigned before its first use.
        names = re.findall(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)", joined)
        self.assertLessEqual(set(names), {"name", "EUID", "UID", "OSTYPE", "locations", "newline", "HOME",
                                          "repository", "TMPDIR"})
        for name in ("HOME", "TMPDIR", "repository", "locations", "newline"):
            self.assertLess(re.search(rf"(?m)^{name}=", joined).start(),
                            re.search(r"\$\{?" + name + r"\b", joined).start(), name)
        self.assertIn("scripts/isolation-sign", ceremony_files())
        # Tracked as executable (mode 100755) in the git index.
        listing = subprocess.run([GIT, "-c", "core.fsmonitor=", "-c", "core.hooksPath=/dev/null", "ls-files", "-s",
                                  "--", "scripts/isolation-sign"], cwd=ROOT, capture_output=True, text=True,
                                 env=dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1"))
        self.assertEqual(listing.returncode, 0, listing.stderr)
        self.assertRegex(listing.stdout, r"^100755 [0-9a-f]{40,64} 0\tscripts/isolation-sign\n$")

    @unittest.skipUnless(LAUNCHER_WORKS, "needs /usr/bin/env -S, /bin/bash, /usr/bin/python3 and a non-root user")
    def test_launcher_starts_the_ceremony_clean_whatever_the_caller_exports(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            repo = launcher_layout(base)
            launcher = repo / "scripts/isolation-sign"
            write_fakes(base / "fakes", base / "marker")
            environment, marker = hostile_environment(base, PATH=f"{base / 'fakes'}:/usr/bin:/bin")

            def start(*arguments, command=None, cwd=repo, env=environment):
                controller, terminal = os.openpty()
                try:
                    return subprocess.run(command or [str(launcher), *arguments], cwd=cwd, env=env, stdin=terminal,
                                          capture_output=True, text=True, timeout=60)
                finally:
                    os.close(controller)
                    os.close(terminal)
            # None of the exported functions, BASH_ENV or variables reaches the ceremony: it gets
            # to its normal next refusal (this layout has no production pin) and nothing ran.
            result = start(*self.required())
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("there is no production root pin", result.stderr, result.stderr)
            self.assertNotIn("unexpected environment variable", result.stderr)
            self.assertNotIn("is set: key locations", result.stderr)
            result = start("--help")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("STAGE 1", result.stdout)
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
            self.assertFalse((base / "marker").exists())
            # Started without its first line (through bash): refused, nothing else runs.
            result = start(command=[BASH, str(launcher), "--help"], env=clean_environment())
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("start the ceremony as scripts/isolation-sign", result.stderr)
            # Only from the repository top level, and never through a symlink.
            result = start("--help", cwd=base)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("repository top level", result.stderr)
            link = base / "isolation-sign"
            link.symlink_to(launcher)
            result = start(command=[str(link), "--help"])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("symlinked launcher", result.stderr)
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")

    # ---------------------------------------------------------------- stage 2 runs only from a valid snapshot

    def test_stage_2_refuses_without_a_valid_snapshot(self):
        with tempfile.TemporaryDirectory() as temporary:
            fake = Path(temporary).resolve() / "not-a-snapshot"
            fake.mkdir()
            (fake / "keep").write_text("x")
            markers = {"DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT": str(fake), "DUDUCLAW_ISOLATION_CEREMONY_REPO": str(ROOT)}
            for env in (markers, {"DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT": str(fake)},
                        {"DUDUCLAW_ISOLATION_CEREMONY_REPO": str(ROOT)}):
                with self.subTest(env=sorted(env)):
                    result = self.run_sign(*self.required(), env=env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("not launched from a valid ceremony snapshot", result.stderr)
                    self.assertTrue((fake / "keep").exists())  # never removed

    @unittest.skipUnless(CEREMONY_TOOLS, "needs env, bash, git, python3 (3.9+) ... in system directories")
    def test_stage_2_refuses_a_snapshot_that_is_writable_or_holds_other_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            snapshot = base / "snapshot"
            (snapshot / "scripts").mkdir(parents=True)
            shutil.copyfile(RELEASE, snapshot / "scripts/release.sh")
            (snapshot / "extra").write_text("x")
            env = clean_environment(DUDUCLAW_ISOLATION_CEREMONY_SNAPSHOT=str(snapshot),
                                    DUDUCLAW_ISOLATION_CEREMONY_REPO=str(ROOT))
            result = subprocess.run([BASH, str(snapshot / "scripts/release.sh"), "isolation-sign", *self.required()],
                                    cwd=ROOT, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                    timeout=120)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not a valid ceremony snapshot", result.stderr)
            self.assertTrue((snapshot / "extra").exists())

    def snapshot_audit(self, snapshot, script, files):
        return subprocess.run([sys.executable, "-I", "-B", "-c", python_constant("ISOLATION_SNAPSHOT_AUDIT_PY"),
                               str(snapshot), str(script), *files], capture_output=True, text=True, timeout=60)

    def test_snapshot_audit_accepts_exactly_the_stage_1_layout(self):
        files = ["scripts/release.sh", "release/isolation/a.json", "Cargo.toml"]
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            source = base / "source"
            for path in files:
                (source / path).parent.mkdir(parents=True, exist_ok=True)
                (source / path).write_text(path)
            snapshot = make_snapshot(source, base / "snapshot", files)
            script = snapshot / "scripts/release.sh"
            try:
                result = self.snapshot_audit(snapshot, script, files)
                self.assertEqual(result.returncode, 0, result.stderr)
                for case, arguments in (("missing file", (snapshot, script, [*files, "scripts/other.py"])),
                                        ("unlisted file", (snapshot, script, files[:-1])),
                                        ("another script", (snapshot, source / "scripts/release.sh", files)),
                                        ("relative snapshot", (Path("snapshot"), script, files))):
                    with self.subTest(case=case):
                        self.assertNotEqual(self.snapshot_audit(*arguments).returncode, 0)
                os.chmod(snapshot / "release/isolation", 0o700)
                os.chmod(snapshot / "release/isolation/a.json", 0o600)
                result = self.snapshot_audit(snapshot, script, files)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("writable", result.stderr)
                os.chmod(snapshot / "release/isolation/a.json", 0o400)
                (snapshot / "release/isolation/link").symlink_to(snapshot / "Cargo.toml")
                self.assertNotEqual(self.snapshot_audit(snapshot, script, files).returncode, 0)
            finally:
                make_writable(base)

    def test_cleanup_removes_snapshot_and_work_on_success_failure_and_signal(self):
        # Stage 1 owns (and removes) the snapshot; stage 2 removes only its work directory.
        for owner, ending in [(owner, ending) for owner in ("1", "0")
                              for ending in ("exit 0", "exit 1", "kill -TERM $$; sleep 5")]:
            with self.subTest(owner=owner, ending=ending), tempfile.TemporaryDirectory() as temporary:
                base = Path(temporary).resolve()
                (base / "source").mkdir()
                (base / "source/Cargo.toml").write_text("x")
                snapshot = make_snapshot(base / "source", base / "snapshot", ["Cargo.toml"])
                work = base / "work"
                (work / "tmp").mkdir(parents=True)
                (work / "root").mkdir()
                (work / "root/root.json").write_text("{}")
                os.chmod(work / "root/root.json", 0o400)
                os.chmod(work / "root", 0o500)
                result = run_section(
                    f'ISOLATION_TOOL_ENV="{system_tool("env")}" ISOLATION_TOOL_RM="{system_tool("rm")}"\n'
                    f'ISOLATION_TOOL_CHMOD="{system_tool("chmod")}"\n'
                    f'ISOLATION_SNAPSHOT="{snapshot}" ISOLATION_WORK="{work}" ISOLATION_OWNS_SNAPSHOT={owner}\n'
                    f'isolation_set_traps\n{ending}\n')
                try:
                    self.assertEqual(snapshot.exists(), owner == "0", result.stderr)
                    self.assertFalse(work.exists(), result.stderr)  # also its read-only root copies
                    self.assertTrue((base / "source").exists())
                finally:
                    if snapshot.exists():
                        make_writable(snapshot)

    # ---------------------------------------------------------------- stage 2 gate on the original repository

    def make_tag_repository(self, directory, files):
        """A throwaway repository whose HEAD is tag v9.9.9, isolated from any user/system git config."""
        repo = Path(directory).resolve() / "repo"
        repo.mkdir()
        env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")

        def git(*args):
            # The test's own git calls never run the hostile hooks/fsmonitor/filter set up later.
            neutral = ["-c", "core.fsmonitor=", "-c", "core.hooksPath=/dev/null", "-c", "filter.evil.clean=cat",
                       "-c", "filter.evil.smudge=cat", "-c", "filter.evil.required=false"]
            return subprocess.run([GIT, *neutral, *args], cwd=repo, env=env, check=True, capture_output=True,
                                  text=True).stdout.strip()
        git("init", "-q")
        for key, value in (("user.name", "ceremony test"), ("user.email", "ceremony@example.invalid"),
                           ("commit.gpgsign", "false"), ("tag.gpgsign", "false")):
            git("config", key, value)
        for path, content in files.items():
            (repo / path).parent.mkdir(parents=True, exist_ok=True)
            (repo / path).write_bytes(content)
            if path == "scripts/isolation-sign":
                (repo / path).chmod(0o755)  # committed as 100755, like the real launcher
        (repo / ".gitignore").write_text("__pycache__/\n*.pyc\ntarget/\n")
        git("add", ".")
        git("commit", "-qm", "ceremony fixture")
        git("tag", "v9.9.9")
        return repo, git

    def hostile_git_config(self, repo, git, base):
        """Repository-local config that would run a marker script from any hook, fsmonitor,
        pager or filter (set after the commit, as an attacker with write access would)."""
        marker = base / "git-marker"
        hostile = base / "hostile.sh"
        hostile.write_text(f'#!/bin/sh\necho "hostile $0 $*" >> "{marker}"\ncat\n')
        hostile.chmod(0o755)
        hooks = base / "hooks"
        hooks.mkdir()
        for hook in ("post-checkout", "pre-commit", "post-index-change", "reference-transaction", "fsmonitor-watchman"):
            (hooks / hook).write_text(f'#!/bin/sh\necho "hook {hook}" >> "{marker}"\n')
            (hooks / hook).chmod(0o755)
        for key, value in (("core.fsmonitor", str(hostile)), ("core.hooksPath", str(hooks)),
                           ("core.pager", str(hostile)), ("filter.evil.clean", str(hostile)),
                           ("filter.evil.smudge", str(hostile)), ("filter.evil.required", "true"),
                           ("core.sshCommand", str(hostile)), ("credential.helper", str(hostile))):
            git("config", key, value)
        (repo / ".git/info").mkdir(exist_ok=True)
        (repo / ".git/info/attributes").write_text("* filter=evil\n")
        return marker

    def check_repository(self, repo, snapshot, files, env=None):
        return run_section(
            f'ISOLATION_TOOL_ENV="{system_tool("env")}" ISOLATION_TOOL_GIT="{GIT}" '
            f'ISOLATION_TOOL_PYTHON="{sys.executable}"\n'
            f'ISOLATION_CEREMONY_FILES=({" ".join(files)})\nISOLATION_SNAPSHOT="{snapshot}"\n'
            'isolation_check_repository v9.9.9\n', cwd=repo, env=env)

    def test_stage_2_gate_is_head_at_tag_and_snapshot_bytes_equal_the_tag_blobs(self):
        files = ["scripts/sign-isolation-manifest.py", "scripts/package-linux-release.py",
                 "release/isolation/manifest-template.json"]
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            repo, git = self.make_tag_repository(directory, {
                "scripts/sign-isolation-manifest.py": b"import hashlib\n",
                "scripts/package-linux-release.py": b"import subprocess\n",
                "release/isolation/manifest-template.json": b"{}\n"})
            snapshot = make_snapshot(repo, base / "snapshot", files)
            marker = self.hostile_git_config(repo, git, base)
            try:
                env = clean_environment(GIT_DIR=str(base / "nowhere"), GIT_WORK_TREE=str(base),
                                        GIT_CONFIG_PARAMETERS=f"'core.fsmonitor'='{base / 'hostile.sh'}'")
                result = self.check_repository(repo, snapshot, files, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                # Nothing of the working tree is used after stage 1, so its state is irrelevant:
                # changes, untracked/ignored files and index flags do not matter.
                (repo / "scripts/sign-isolation-manifest.py").write_text("raise SystemExit('tampered')\n")
                (repo / "scripts/hashlib.py").write_text("raise SystemExit('planted')\n")
                (repo / "scripts/__pycache__").mkdir()
                git("update-index", "--assume-unchanged", "scripts/package-linux-release.py")
                result = self.check_repository(repo, snapshot, files, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
                # The snapshot must be byte-identical to the tag's blobs.
                os.chmod(snapshot / "release/isolation", 0o700)
                os.chmod(snapshot / "release/isolation/manifest-template.json", 0o600)
                (snapshot / "release/isolation/manifest-template.json").write_text('{"changed":1}\n')
                result = self.check_repository(repo, snapshot, files, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("differs from tag v9.9.9", result.stderr)
                (snapshot / "release/isolation/manifest-template.json").write_text("{}\n")
                # Every listed file must be a regular file tracked at the tag.
                result = self.check_repository(repo, snapshot, [*files, "scripts/absent.py"], env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("scripts/absent.py is not a regular file tracked at the tag", result.stderr)
                # HEAD must be the tag itself (a new commit made with plumbing only: no index,
                # no working tree, so no filter of the hostile config runs for the test itself).
                after = git("commit-tree", git("rev-parse", "HEAD^{tree}"), "-p", "HEAD", "-m", "after the tag")
                git("update-ref", "HEAD", after)
                result = self.check_repository(repo, snapshot, files, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("check out tag v9.9.9 first", result.stderr)
                self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
            finally:
                make_writable(snapshot)

    def test_blob_check_recomputes_git_blob_ids(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            (base / "Cargo.toml").write_bytes(b"x\n")
            sha1 = hashlib.sha1(b"blob 2\0x\n").hexdigest()
            sha256 = hashlib.sha256(b"blob 2\0x\n").hexdigest()
            code = python_constant("ISOLATION_BLOB_CHECK_PY")
            for expected, ok in ((sha1, True), (sha256, True), ("0" * 40, False), ("0" * 64, False)):
                result = subprocess.run([sys.executable, "-I", "-B", "-c", code, str(base), f"Cargo.toml={expected}"],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, ok, result.stderr)

    # ---------------------------------------------------------------- end to end: the real two-stage run

    def ceremony_repository(self, base, tag_release=None):
        """The real ceremony files at tag v9.9.9 with a production pin of a root signed by two
        of three fresh ssh-keygen keys, and hostile repository-local git configuration.
        `tag_release` replaces scripts/release.sh AT THE TAG only (the snapshot, i.e. stage 2)."""
        files = ceremony_files()
        contents = {path: (ROOT / path).read_bytes() for path in files
                    if path not in ("Cargo.toml", "release/bootstrap-root.sha256")}
        if tag_release is not None:
            contents["scripts/release.sh"] = tag_release
        contents["Cargo.toml"] = b'[workspace]\n\n[workspace.package]\nversion = "9.9.9"\n'
        keys = base / "keys"
        keys.mkdir()
        roots = []
        for name in ("r1", "r2", "r3"):
            subprocess.run([SSH_KEYGEN, "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", str(keys / name)],
                           check=True, capture_output=True)
            roots.append(META.parse_public_key((keys / (name + ".pub")).read_text(), META.ROOT_APPLICATION))
        document = {"schema_version": 1, "version": 1, "expires_at_unix": 4_000_000_000, "root_threshold": 2,
                    "root_keys": roots, "target_key": roots[-1], "revoked_key_ids": [], "frozen": False}
        unsigned = base / "root.unsigned.json"
        unsigned.write_text(json.dumps(document))
        root = base / "root.json"
        signing = subprocess.run([sys.executable, "-I", str(SIGNER), "--kind", "root", "--template", str(unsigned),
                                  "--output", str(root), "--key-count", "2", "--ssh-keygen", SSH_KEYGEN],
                                 input=f"{keys / 'r1'}\n{keys / 'r2'}\n", capture_output=True, text=True,
                                 env=clean_environment())
        self.assertEqual(signing.returncode, 0, signing.stderr)
        contents["release/bootstrap-root.sha256"] = hashlib.sha256(root.read_bytes()).hexdigest().encode() + b"\n"
        repo, git = self.make_tag_repository(base, contents)
        git_marker = self.hostile_git_config(repo, git, base)
        # After stage 1's snapshot the working tree is irrelevant: tamper with it now. The
        # working-tree release.sh still runs stage 1, but its stage-2 text is never used.
        script = repo / "scripts/release.sh"
        script.write_text(RELEASE.read_text().replace("--output-dir must not exist yet", "TAMPERED-WORKING-TREE"))
        (repo / "scripts/sign-isolation-manifest.py").write_text(
            f'open("{base / "marker"}", "a").write("working-tree signer ran\\n")\n')
        (repo / "scripts/hashlib.py").write_text(f'open("{base / "marker"}", "a").write("planted\\n")\n')
        return repo, root, Path(str(root) + ".signatures.json"), git_marker

    def run_ceremony(self, repo, base, *arguments, launcher_env=None):
        """The two-stage ceremony through the real launcher, with hostile tools in the caller's PATH.
        Return the leaked ceremony directories as well, checking the launcher's actual temp locations."""
        marker = base / "marker"
        write_fakes(base / "fakes", marker)
        if launcher_env is None:
            environment = clean_environment(PATH=f"{base / 'fakes'}:/usr/bin:/bin:/usr/sbin:/sbin")
        else:
            environment = launcher_env
        command = [str(repo / "scripts/isolation-sign"), *arguments]
        before = ceremony_leftovers()
        controller, terminal = os.openpty()
        try:
            result = subprocess.run(command, cwd=repo, env=environment, stdin=terminal, capture_output=True,
                                    text=True, timeout=300)
        finally:
            os.close(controller)
            os.close(terminal)
        return result, marker, ceremony_leftovers() - before

    @unittest.skipUnless(CEREMONY_TOOLS, "needs env, bash, git, python3 (3.9+), ssh-keygen in system/Homebrew dirs")
    def test_end_to_end_stage_2_runs_the_snapshot_never_path_or_working_tree_and_cleans_up(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            repo, root, signatures, git_marker = self.ceremony_repository(base)
            output = base / "out"
            output.mkdir()  # stage 2 refuses here, after every gate and the root verification
            result, marker, leftovers = self.run_ceremony(repo, base, "--root-metadata", str(root),
                                                        "--root-signatures", str(signatures),
                                                        "--output-dir", str(output))
            report = result.stdout + result.stderr
            self.assertNotEqual(result.returncode, 0, report)
            self.assertIn("Stage 1: snapshot of v9.9.9", result.stdout, report)
            self.assertIn("Stage 2: running from", result.stdout, report)
            self.assertIn("root threshold met by 2 distinct root keys; every signature verified", result.stdout,
                          report)
            self.assertIn("--output-dir must not exist yet", result.stderr, report)
            self.assertNotIn("TAMPERED-WORKING-TREE", report)
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
            self.assertFalse(git_marker.exists(), git_marker.read_text() if git_marker.exists() else "")
            tools = dict(part.split("=", 1) for part in
                         re.search(r"Tools: (.*)", result.stdout)[1].split() if "=" in part)
            for name in ("env", "git", "python3", "ssh-keygen"):
                self.assertTrue(tools[name].startswith("/"), tools)
                self.assertNotIn(str(base), tools[name])
            # The snapshot and the work directory are gone after the failure.
            self.assertEqual(leftovers, set())

    @unittest.skipUnless(CEREMONY_TOOLS and OPTIONAL_TOOLS, "needs every ceremony tool incl. gh, minisign, docker")
    def test_end_to_end_preflight_succeeds_and_cleans_up(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            repo, root, signatures, git_marker = self.ceremony_repository(base)
            result, marker, leftovers = self.run_ceremony(repo, base, "--root-metadata", str(root),
                                                        "--root-signatures", str(signatures),
                                                        "--output-dir", str(base / "out"), "--preflight")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("Preflight passed for v9.9.9", result.stdout)
            self.assertFalse((base / "out").exists())
            self.assertFalse(marker.exists())
            self.assertFalse(git_marker.exists())
            self.assertEqual(leftovers, set())

    @unittest.skipUnless(CEREMONY_TOOLS, "needs env, bash, git, python3 (3.9+), ssh-keygen in system/Homebrew dirs")
    def test_end_to_end_root_signature_with_a_flipped_byte_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            repo, root, signatures, _ = self.ceremony_repository(base)
            armors = json.loads(signatures.read_text())
            lines = armors[0].splitlines()
            blob = bytearray(base64.b64decode("".join(lines[1:-1])))
            blob[-32] ^= 1  # lowest byte of S: still canonical, no longer a valid signature
            encoded = base64.b64encode(bytes(blob)).decode()
            armors[0] = ("-----BEGIN SSH SIGNATURE-----\n" + "\n".join(encoded[i:i + 70] for i in
                         range(0, len(encoded), 70)) + "\n-----END SSH SIGNATURE-----\n")
            tampered = base / "tampered.signatures.json"
            tampered.write_text(json.dumps(armors))
            result, marker, leftovers = self.run_ceremony(repo, base, "--root-metadata", str(root),
                                                        "--root-signatures", str(tampered),
                                                        "--output-dir", str(base / "out"))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not verify", result.stderr)
            self.assertIn("--root-signatures do not meet the root threshold", result.stderr)
            self.assertEqual(leftovers, set())

    @unittest.skipUnless(CEREMONY_TOOLS and LAUNCHER_WORKS,
                         "needs every required ceremony tool and a working /usr/bin/env -S launcher")
    def test_end_to_end_through_the_launcher_nothing_the_caller_exports_runs(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            repo, root, signatures, git_marker = self.ceremony_repository(base)
            output = base / "out"
            output.mkdir()  # the normal refusal after every gate and the root verification
            environment, hostile_marker = hostile_environment(
                base, PATH=f"{base / 'fakes'}:/usr/bin:/bin:/usr/sbin:/sbin",
                GIT_CONFIG_PARAMETERS=f"'core.fsmonitor'='{base / 'hostile.sh'}'")
            before = ceremony_leftovers()
            result, marker, _ = self.run_ceremony(repo, base, "--root-metadata", str(root), "--root-signatures",
                                                  str(signatures), "--output-dir", str(output),
                                                  launcher_env=environment)
            report = result.stdout + result.stderr
            self.assertNotEqual(result.returncode, 0, report)
            self.assertIn("Stage 1: snapshot of v9.9.9", result.stdout, report)
            self.assertIn("Stage 2: running from", result.stdout, report)
            self.assertIn("root threshold met by 2 distinct root keys", result.stdout, report)
            self.assertIn("--output-dir must not exist yet", result.stderr, report)
            for path in (hostile_marker, marker, git_marker):
                self.assertFalse(path.exists(), path.read_text() if path.exists() else "")
            self.assertEqual(ceremony_leftovers() - before, set())

    @unittest.skipUnless(CEREMONY_TOOLS, "needs env, bash, git, python3 (3.9+), ssh-keygen in system/Homebrew dirs")
    def test_stage_1_removes_the_snapshot_when_stage_2_fails_before_its_own_trap(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            text = RELEASE.read_text()
            # The tag's (snapshot's) stage 2 stops at its very first statement, before any trap.
            failing = text.replace("isolation_stage2() {\n",
                                   'isolation_stage2() {\n    echo "stage 2 stopped at once" >&2\n    return 3\n', 1)
            self.assertNotEqual(failing, text)
            repo, root, signatures, _ = self.ceremony_repository(base, tag_release=failing.encode())
            result, marker, leftovers = self.run_ceremony(repo, base, "--root-metadata", str(root),
                                                        "--root-signatures", str(signatures),
                                                        "--output-dir", str(base / "out"))
            report = result.stdout + result.stderr
            self.assertEqual(result.returncode, 3, report)
            self.assertIn("Stage 1: snapshot of v9.9.9", result.stdout, report)
            self.assertIn("stage 2 stopped at once", result.stderr, report)
            self.assertEqual(leftovers, set())  # stage 1's own trap removed the snapshot
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
        # Stage 1 waits for stage 2 as a child; it never execs it.
        stage1 = shell_function("isolation_stage1")
        self.assertNotIn("exec ", stage1)
        self.assertIn('"$ISOLATION_SNAPSHOT/scripts/release.sh" isolation-sign "$@" || status=$?', stage1)
        self.assertLess(stage1.index("ISOLATION_OWNS_SNAPSHOT=1"), stage1.index("isolation_set_traps"))
        self.assertLess(stage1.index("isolation_set_traps"), stage1.index('ISOLATION_SNAPSHOT="$(isolation_run'))
        self.assertNotIn("ISOLATION_OWNS_SNAPSHOT", shell_function("isolation_stage2"))

    # ---------------------------------------------------------------- FIDO2 helper, target key, root copies

    def test_sk_helper_is_resolved_only_next_to_the_selected_ssh_keygen(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            # Homebrew-like: <prefix>/bin/ssh-keygen -> <prefix>/Cellar/openssh/9/bin/ssh-keygen.
            prefix = base / "brew"
            keg = prefix / "Cellar/openssh/9"
            for directory in (prefix / "bin", prefix / "libexec", keg / "bin", keg / "libexec"):
                directory.mkdir(parents=True, exist_ok=True)
            for directory, _, _ in os.walk(prefix):
                os.chmod(directory, 0o755)
            real = keg / "bin/ssh-keygen"
            real.write_text("#!/bin/sh\nexit 1\n")
            real.chmod(0o755)
            named = prefix / "bin/ssh-keygen"
            named.symlink_to(real)

            def resolve():
                return run_section(f'ISOLATION_TOOL_ENV="{system_tool("env")}" ISOLATION_TOOL_PYTHON="{sys.executable}"\n'
                                   f'ISOLATION_TOOL_SSH_KEYGEN="{real}"\nisolation_resolve_sk_helper "{named}"\n'
                                   'printf "%s\\n" "${ISOLATION_TOOL_SSH_SK_HELPER:-none}"\n')

            def helper(path):
                path.write_text("#!/bin/sh\nexit 1\n")
                path.chmod(0o755)
                return path
            result = resolve()
            self.assertEqual((result.returncode, result.stdout), (0, "none\n"), result.stderr)
            prefix_helper = helper(prefix / "libexec/ssh-sk-helper")
            result = resolve()
            self.assertEqual((result.returncode, result.stdout), (0, f"{prefix_helper}\n"), result.stderr)
            # The keg's own libexec (the compiled-in default of that ssh-keygen) comes first.
            keg_helper = helper(keg / "libexec/ssh-sk-helper")
            result = resolve()
            self.assertEqual((result.returncode, result.stdout), (0, f"{keg_helper}\n"), result.stderr)
            # A present helper that fails the audit refuses; it is never skipped for another.
            keg_helper.chmod(0o777)
            result = resolve()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("failed the location audit", result.stderr)
        # Stage 2 hands it to the signers only as --ssh-sk-helper (they set SSH_SK_HELPER for
        # ssh-keygen -Y sign alone); verification calls never get it.
        body = re.sub(r"\\\n\s*", "", isolation_function())
        self.assertIn('signing_tools+=(--ssh-sk-helper "$ISOLATION_TOOL_SSH_SK_HELPER")', body)
        self.assertEqual(body.count('"${signing_tools[@]}"'), 2)
        for line in body.splitlines():
            if "--check-threshold" in line or "--check-target" in line:
                self.assertNotIn("signing_tools", line)
        # Never in the shared child environment of the ceremony.
        self.assertNotIn("SSH_SK_HELPER", shell_function("isolation_child_environment"))
        self.assertNotIn("SSH_SK_HELPER=$ISOLATION_TOOL_SSH_SK_HELPER", section_code())

    def test_target_key_is_verified_before_anything_joins_the_upload_list(self):
        body = re.sub(r"\\\n\s*", "", isolation_function())
        self.assertIn('--signing-key-stdin "${signing_tools[@]}" --target-root "$root_metadata" '
                      '--target-root-sha256 "$pin"', body)
        handoff = body.index('isolation_handoff 1 "$target_key"')
        check = body.index('--kind release --check-target "$staging/isolated-linux-$arch-manifest.json" '
                           '--root-metadata "$root_metadata" --root-sha256 "$pin" --ssh-keygen "$ssh_keygen"')
        delete = body.index('isolation_run "$ISOLATION_TOOL_RM" -rf -- "$staging/isolated-linux-$arch-"*')
        self.assertLess(handoff, check)
        self.assertLess(check, delete)
        self.assertLess(delete, body.index("uploads+="))
        packager = (ROOT / "scripts/package-linux-release.py").read_text()
        self.assertIn("target_root=args.target_root or args.bootstrap_root", packager)
        self.assertIn('signer_arguments += ["--root-metadata", str(Path(target_root).resolve())]', packager)
        self.assertIn("expected_root_sha256=args.target_root_sha256", packager)

    def test_root_metadata_is_copied_once_and_only_the_verified_copies_are_read(self):
        body = isolation_function()
        copy = body.index('root_metadata="$ISOLATION_WORK/root/root.json"')
        self.assertIn('root_signatures="$ISOLATION_WORK/root/root.json.signatures.json"', body[copy:copy + 120])
        before, after = body[:copy], body[copy:]
        # Before the switch, the given paths are only checked to be regular files and copied once.
        for variable in ("root_metadata", "root_signatures"):
            self.assertEqual(before.count(f'"${variable}"'), 3, variable)
            self.assertEqual(body.count(f"ISOLATION_{variable.upper()}"), 1, variable)
        self.assertIn('"$ISOLATION_TOOL_CP" -- "$root_metadata" "$ISOLATION_WORK/root/root.json"', before)
        self.assertIn('"$ISOLATION_TOOL_CHMOD" -R u=rX,go= -- "$ISOLATION_WORK/root"', before)
        # Pin, threshold and ssh-keygen -Y verify run on the copies, and so does everything later.
        for needle in ('isolation_sha256 "$root_metadata"', '--check-threshold "$root_metadata" --signatures '
                       '"$root_signatures"', '--bootstrap-root "$root_metadata" --bootstrap-root-signatures '
                       '"$root_signatures"', '--root-metadata "$root_metadata" --root-sha256 "$pin"'):
            self.assertIn(needle, after)
            self.assertNotIn(needle, before)
        self.assertLess(body.index('isolation_check_repository "$tag"'), copy)

    # ---------------------------------------------------------------- --output-dir: private staging, copy last

    def output_section(self, command):
        return run_section(f'ISOLATION_TOOL_ENV="{system_tool("env")}" ISOLATION_TOOL_PYTHON="{sys.executable}"\n'
                           + command)

    def refused_output_parents(self, base):
        """(case, --output-dir, message) for each parent the rule refuses: group/world-writable (also
        sticky), a symlink, missing, and (unless root) a directory that is not yours."""
        mine = "owned by you that is not group/other-writable"
        cases = []
        for name, mode in (("group-writable", 0o770), ("world-writable", 0o777), ("sticky-world-writable", 0o1777)):
            directory = base / name
            directory.mkdir()
            directory.chmod(mode)
            cases.append((name, directory / "out", mine))
        real = base / "real-parent"
        real.mkdir(mode=0o700)
        (base / "link-parent").symlink_to(real)
        cases.append(("symlink", base / "link-parent" / "out", "its parent directory is a symbolic link"))
        cases.append(("missing", base / "missing-parent" / "out", "its parent directory does not exist"))
        if os.geteuid() != 0:
            cases.append(("not yours", Path("/usr") / f"duduclaw-isolation-test-out-{os.getpid()}", mine))
        return cases

    def test_output_dir_parent_must_be_yours_and_not_writable_by_others(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            for case, output, message in self.refused_output_parents(base):
                with self.subTest(case=case):
                    result = self.output_section(f'isolation_check_output_dir "{output}"\n')
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(os.path.lexists(output))
            # An existing --output-dir (also a dangling link) is refused as before.
            existing = base / "real-parent" / "existing"
            existing.mkdir()
            dangling = base / "real-parent" / "dangling"
            dangling.symlink_to(base / "nowhere")
            for output in (existing, dangling):
                result = self.output_section(f'isolation_check_output_dir "{output}"\n')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--output-dir must not exist yet", result.stderr)
            result = self.output_section(f'isolation_check_output_dir "{base / "real-parent" / ".." / "x"}"\n')
            self.assertNotEqual(result.returncode, 0)
            # A new directory under a parent of yours that only you can write: accepted, not created.
            result = self.output_section(f'isolation_check_output_dir "{base / "real-parent" / "out"}"\n')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((base / "real-parent" / "out").exists())

    def test_copy_into_output_dir_is_exclusive_private_and_digest_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            staging = base / "staging"
            staging.mkdir(mode=0o700)
            files = {"isolated-linux-x86_64-manifest.json": b"{}", "duduclaw-bootstrap-linux-x86_64": b"\x7fELF",
                     "SHA256SUMS-isolation.txt": b"sums\n"}
            for name, data in files.items():
                (staging / name).write_bytes(data)
            (staging / "duduclaw-bootstrap-linux-x86_64").chmod(0o755)
            entries = [f"{name}={hashlib.sha256(data).hexdigest()}" for name, data in files.items()]
            parent = base / "parent"
            parent.mkdir(mode=0o700)

            def copy(output, items):
                return self.output_section(f'isolation_copy_results "{staging}" "{output}" '
                                           + " ".join(f'"{item}"' for item in items) + "\n")
            result = copy(parent / "out", entries)
            self.assertEqual(result.returncode, 0, result.stderr)
            output = parent / "out"
            self.assertEqual(stat.S_IMODE(output.lstat().st_mode), 0o700)
            self.assertEqual(sorted(path.name for path in output.iterdir()), sorted(files))
            for name, data in files.items():
                self.assertEqual(stat.S_IMODE((output / name).lstat().st_mode), 0o600, name)
                self.assertEqual((output / name).read_bytes(), data)
            # Never into an existing directory.
            result = copy(output, entries)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must not exist yet", result.stderr)
            self.assertIn("NOT copied into --output-dir", result.stderr)
            # Staged bytes that are not the recorded (uploaded) digest are refused and reported.
            result = copy(parent / "mismatch", [*entries[:-1], "SHA256SUMS-isolation.txt=" + "0" * 64])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("is not the uploaded/verified bytes", result.stderr)
            self.assertIn("nothing of the upload or its read-back depends on that copy", result.stderr)
            # A staged file that became a link is never followed.
            (staging / "SHA256SUMS-isolation.txt").unlink()
            (staging / "SHA256SUMS-isolation.txt").symlink_to(base / "elsewhere")
            (base / "elsewhere").write_bytes(b"sums\n")
            result = copy(parent / "linked", entries)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("NOT copied into --output-dir", result.stderr)
            (staging / "SHA256SUMS-isolation.txt").unlink()
            (staging / "SHA256SUMS-isolation.txt").write_bytes(b"sums\n")
            # The parent is checked again right before the copy.
            late = base / "late"
            late.mkdir(mode=0o700)
            for case, target, message in self.refused_output_parents(late):
                with self.subTest(case=case):
                    result = copy(target, entries)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertFalse(os.path.lexists(target))

    def test_everything_from_signing_to_upload_uses_only_the_private_staging_directory(self):
        body = re.sub(r"\\\n\s*", "", isolation_function())
        verify = re.sub(r"\\\n\s*", "", shell_function("isolation_verify_staged"))
        self.assertIn('local staging="$ISOLATION_WORK/staging"', body)
        self.assertIn('isolation_run "$ISOLATION_TOOL_MKDIR" -m 0700 -- "$staging"', body)
        prompt = body.index('isolation_key_location "target key handle"')
        copy = body.index('isolation_copy_results "$staging" "$output_dir" "${copies[@]}" || return 1')
        # --output-dir: the early check (before --preflight ends and before any prompt), then only the copy.
        early = body.index('isolation_check_output_dir "$output_dir" || return 1')
        self.assertLess(early, body.index('if [[ "$ISOLATION_PREFLIGHT" == "1" ]]; then'))
        self.assertLess(early, prompt)
        uses = [match.start() for match in re.finditer(r"\$\{?output_dir\b", body)]
        self.assertEqual([use for use in uses if early + 40 < use < copy], [])
        self.assertNotIn("output_dir", verify)
        # Every path a helper or tool is given from the first prompt to the copy (and in the
        # pre-upload re-verification) lies in the private staging or work directory.
        flags = (r"(--output-dir|--from-release-archive|--extract-bootstrap|--check-target|--artifacts-dir|--bundle"
                 r"|--migration-bundle-from|--minisign-sign|--output|--dir|-m|-x) \"([^\"]*)\"")
        paths = re.findall(flags, body[prompt:copy] + "\n" + verify)
        self.assertGreaterEqual(len(paths), 20)
        for flag, path in paths:
            self.assertTrue(path.startswith(("$staging", "$ISOLATION_WORK/", "$authorization")), (flag, path))
        self.assertIn('local authorization="$staging/$bundle_name.authorization.json"', body)
        self.assertIn('local sums_file="$staging/SHA256SUMS-isolation.txt"', body)
        for entry in re.findall(r"uploads\+=\(([^)]*)\)", body):
            for path in re.findall(r'"([^"]*)"', entry):
                self.assertTrue(path.startswith(("$staging/", "$asset")), path)
        self.assertIn('for asset in "$staging/isolated-linux-$arch-"*; do', body)
        # Deletion on failure touches only the staging directory.
        deletions = re.findall(r'"\$ISOLATION_TOOL_RM" -r?f -- "([^"]*)"', body)
        self.assertEqual(len(deletions), 2)
        for path in deletions:
            self.assertTrue(path.startswith(("$staging/", "$authorization")), path)
        # Order: UPLOAD answer, re-verification of the staged bytes, hashes and sums from them,
        # re-hash, upload, read-back, and only then the copy (also after --no-upload's verification).
        answer = body.index('if [[ "$answer" != "UPLOAD" ]]; then')
        reverify = body.index('isolation_verify_staged "$staging" "$pin" "$legacy_public" "$migration_bundle" '
                              '|| return 1')
        hashed = body.index('hashes+=("$(isolation_sha256 "${uploads[$index]}")")')
        sums = body.index('} > "$sums_file"')
        rehash = body.index("changed after it was recorded")
        upload = body.index('isolation_run "$ISOLATION_TOOL_GH" release upload "$tag"')
        readback = body.index("differs from the signed bytes")
        for earlier, later in ((answer, reverify), (reverify, hashed), (hashed, sums), (sums, rehash),
                               (rehash, upload), (upload, readback), (readback, copy),
                               (body.index("--no-upload: nothing uploaded."), copy)):
            self.assertLess(earlier, later)
        self.assertEqual(body.count("isolation_verify_staged"), 1)
        self.assertEqual(body.count("isolation_copy_results"), 1)
        self.assertIn('copies+=("${uploads[$index]##*/}=${hashes[$index]}")', body)
        # The re-verification is the signer's own: target key + every bound digest/size, root
        # threshold + the authorized tar, and the legacy minisign signature, all on staged paths.
        for needle in ('--kind release --check-target "$staging/isolated-linux-$arch-manifest.json" '
                       '--artifacts-dir "$staging" --root-metadata "$root_metadata" --root-sha256 "$pin"',
                       '--kind bundle --check-threshold "$staging/$bundle_name.authorization.json" '
                       '--bundle "$staging/$bundle_name" --root-metadata "$root_metadata" --root-sha256 "$pin"',
                       '-m "$staging/$bundle_name" -x "$staging/$bundle_name.minisig"'):
            self.assertIn(needle, verify)
        self.assertIn('root_metadata="$ISOLATION_WORK/root/root.json"', verify)
        signer = SIGNER.read_text()
        self.assertIn("bind_artifacts(rebound, Path(directory), name[:-len(\"manifest.json\")])", signer)
        self.assertIn("canonical_bytes(bundle_authorization(args.bundle, expires)) != document", signer)
        # The copy: exclusive, never through a link, explicit modes, digests checked.
        program = python_constant("ISOLATION_OUTPUT_PY")
        for needle in ("os.O_NOFOLLOW", "os.O_EXCL", "os.mkdir(name, 0o700, dir_fd=parent_fd)",
                       "os.fchmod(out_fd, 0o700)", "os.fchmod(target_fd, 0o600)", "staged.hexdigest() != expected",
                       "digest_of(copy_fd) != expected", "info.st_uid != uid or info.st_mode & 0o022"):
            self.assertIn(needle, program)
        self.assertIn('copy "$output_dir" "$staging" "$@"', shell_function("isolation_copy_results"))

    @unittest.skipUnless(CEREMONY_TOOLS, "needs env, bash, git, python3 (3.9+), ssh-keygen in system/Homebrew dirs")
    def test_end_to_end_output_parent_is_refused_before_any_prompt(self):
        # --preflight: should the refusal ever regress, the run still ends before any key prompt.
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            repo, root, signatures, git_marker = self.ceremony_repository(base)
            for case, output, message in self.refused_output_parents(base):
                with self.subTest(case=case):
                    result, marker, leftovers = self.run_ceremony(repo, base, "--root-metadata", str(root),
                                                                "--root-signatures", str(signatures),
                                                                "--output-dir", str(output), "--preflight")
                    report = result.stdout + result.stderr
                    self.assertNotEqual(result.returncode, 0, report)
                    self.assertIn("root threshold met by 2 distinct root keys", result.stdout, report)
                    self.assertIn(message, result.stderr, report)
                    self.assertNotIn("Preflight passed", result.stdout, report)
                    self.assertNotIn("no terminal for", result.stderr, report)
                    self.assertFalse(os.path.lexists(output))
                    self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else "")
                    self.assertEqual(leftovers, set())
            self.assertFalse(git_marker.exists(), git_marker.read_text() if git_marker.exists() else "")

    # ---------------------------------------------------------------- R2-2: the docker probe

    def docker_argv(self, binary, image):
        return run_functions(["isolation_fail", "isolation_probe_docker_argv"],
                             'ISOLATION_TOOL_DOCKER=/usr/local/bin/docker\n'
                             f'isolation_probe_docker_argv "{binary}" linux/arm64 "{image}"\n'
                             'printf "%s\\n" "${ISOLATION_DOCKER_ARGV[@]}"\n')

    def test_docker_probe_argv_is_hardened_and_digest_only(self):
        binary = "/tmp/duduclaw-isolation-sign.X/aarch64/probe/duduclaw-bootstrap"
        result = self.docker_argv(binary, DIGEST)
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = result.stdout.splitlines()
        self.assertEqual(argv[:2], ["/usr/local/bin/docker", "run"])
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
        # The probe only ever runs this argv (allow-listed environment); the native path is unchanged.
        probe = shell_function("isolation_probe_pin")
        self.assertIn('isolation_run "${ISOLATION_DOCKER_ARGV[@]}"', probe)
        self.assertIn('"$ISOLATION_TOOL_ENV" -i "$binary" --print-root-pin', probe)
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

    def test_key_locations_come_only_from_the_prompt_never_the_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            key = base / "duduclaw-target-handle"
            key.write_text("handle")
            decoy = base / "decoy"
            decoy.write_text("decoy")
            answers = base / "answers"
            answers.write_text("duduclaw-target-handle\n")
            env = clean_environment(DUDUCLAW_ISOLATION_TARGET_KEY=str(decoy))
            command = (f'ISOLATION_TTY_IN="{answers}" ISOLATION_TTY_OUT=/dev/null\n'
                       'isolation_key_location "target key handle"\n')
            result = run_section(command, cwd=base, env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, f"{key}\n")  # relative answers are made absolute
            answers.write_text("missing-handle\n")
            result = run_section(command, cwd=base, env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("missing-handle", result.stderr)  # messages never name the location
            self.assertIn("target key handle location is not a file", result.stderr)
            result = run_section('ISOLATION_TTY_IN=/nonexistent/tty\nisolation_key_location "target key handle"\n',
                                 cwd=base, env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no terminal for the target key handle prompt", result.stderr)

    def test_handoff_gives_locations_only_on_stdin_and_children_only_the_allow_list(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            record = base / "record.json"
            helper = base / "helper"
            helper.write_text(f"#!{sys.executable} -I\nimport json, os, sys\n"
                              f"json.dump({{'argv': sys.argv, 'env': dict(os.environ), 'stdin': sys.stdin.read()}}, "
                              f"open({str(record)!r}, 'w'))\n")
            helper.chmod(0o755)
            env = clean_environment(SSH_AUTH_SOCK="/agent.sock", DUDUCLAW_SOMETHING="1", PYTHONPATH="/evil",
                                    GIT_DIR="/evil", LD_LIBRARY_PATH="/evil")
            result = run_section(f'ISOLATION_TOOL_ENV="{system_tool("env") or "/usr/bin/env"}"\n'
                                 'HOME=/home/ceremony TERM=xterm-256color LANG=C.UTF-8 LC_ALL="bad value"\n'
                                 'isolation_child_environment\n'
                                 f'isolation_handoff 2 "/keys/r one" /keys/r2 "{helper}" --kind bundle\n', env=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            seen = json.loads(record.read_text())
            self.assertEqual(seen["stdin"], "/keys/r one\n/keys/r2\n")
            self.assertEqual(seen["argv"][1:], ["--kind", "bundle"])
            # Interpreter-added: LC_CTYPE (PEP 538 locale coercion) and macOS's text encoding.
            names = set(seen["env"]) - {"LC_CTYPE", "__CF_USER_TEXT_ENCODING"}
            self.assertEqual(names, {"PATH", "HOME", "TERM", "LANG"})
            self.assertEqual(seen["env"]["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin")
            for value in seen["env"].values():
                self.assertNotIn("/keys/", value)
            self.assertNotIn("/keys/", json.dumps(seen["argv"]))


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
