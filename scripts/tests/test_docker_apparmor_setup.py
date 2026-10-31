"""Exercise setup transactions with mocked Linux/root tools, without loading policy."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "scripts/docker-apparmor-setup.sh").read_text()
PROFILE = (ROOT / "container/apparmor/duduclaw-userns").read_text()
LABEL = "duduclaw-userns (unconfined)\n"


class AppArmorSetupTests(unittest.TestCase):
    def run_case(self, *, existing=None, loaded=False, failure="", success=False):
        with tempfile.TemporaryDirectory(prefix="apparmor-transaction-") as directory:
            root = Path(directory)
            etc = root / "etc/apparmor.d"
            (etc / "abi").mkdir(parents=True)
            (etc / "abi/4.0").touch()
            policy = root / "container/apparmor/duduclaw-userns"
            policy.parent.mkdir(parents=True)
            policy.write_text(PROFILE)
            restriction = root / "restriction"
            restriction.write_text("1")
            profiles = root / "profiles"
            profiles.write_text(LABEL if loaded else "")
            destination = etc / "duduclaw-userns"
            if existing is not None:
                destination.write_text(existing)
            bindir = root / "bin"
            bindir.mkdir()
            python = f"#!{sys.executable}\n"
            helpers = {
                "uname": "#!/bin/sh\necho Linux\n",
                "flock": "#!/bin/sh\nexit 0\n",
                "stat": '#!/bin/sh\ncase "$2" in %u) echo 0;; %a) echo 644;; esac\n',
                "install": python + "import shutil,sys\nshutil.copyfile(sys.argv[-2],sys.argv[-1])\n",
                "mv": python + """import os,signal,sys
if os.environ.get('MOCK_FAIL') == 'rename':
    sys.exit(1)
os.rename(sys.argv[-2], sys.argv[-1])
if os.environ.get('MOCK_FAIL') == 'signal_after_rename':
    os.kill(os.getppid(), signal.SIGTERM)
""",
                "apparmor_parser": python + """import os,sys
from pathlib import Path
root = Path(os.environ['MOCK_ROOT'])
mode = sys.argv[1]
with (root / 'parser_calls').open('a') as calls:
    calls.write(mode + '\\n')
profiles = root / 'profiles'
failure = os.environ.get('MOCK_FAIL', '')
if mode == '-Q':
    sys.exit(1 if failure == 'parse' else 0)
if mode == '-R':
    profiles.write_text('')
    sys.exit(0)
if mode == '-r':
    profiles.write_text('duduclaw-userns (complain)\\n' if failure == 'verify'
                       else 'duduclaw-userns (unconfined)\\n')
    sys.exit(1 if failure == 'load' else 0)
sys.exit(2)
""",
            }
            for program, text in helpers.items():
                file = bindir / program
                file.write_text(text)
                file.chmod(0o755)
            # Substitute only host facilities; all transaction logic runs unchanged.
            script = SOURCE
            replacements = {
                "/etc/apparmor.d": str(etc),
                "/proc/sys/kernel/apparmor_restrict_unprivileged_userns": str(restriction),
                "/sys/kernel/security/apparmor/profiles": str(profiles),
                "if [[ $EUID -ne 0 ]]; then": "if false; then",
                'repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"':
                    f'repo_root="{root}"',
            }
            for original, replacement in replacements.items():
                self.assertIn(original, script)
                script = script.replace(original, replacement)
            executable = root / "setup.sh"
            executable.write_text(script)
            env = {
                **os.environ,
                "PATH": str(bindir) + ":" + os.environ["PATH"],
                "MOCK_ROOT": str(root),
                "MOCK_FAIL": failure,
            }
            result = subprocess.run(
                ["bash", str(executable)], env=env, capture_output=True,
                text=True, timeout=15,
            )
            self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
            if loaded:
                self.assertEqual((root / "parser_calls").read_text().splitlines(), ["-Q"])
            if success:
                self.assertEqual(destination.read_text(), PROFILE)
                self.assertEqual(profiles.read_text(), LABEL)
            else:
                self.assertEqual(destination.exists(), existing is not None)
                if existing is not None:
                    self.assertEqual(destination.read_text(), existing)
                self.assertEqual(profiles.read_text(), LABEL if loaded else "")
            self.assertEqual(list(etc.glob(".duduclaw-userns.*")), [etc / ".duduclaw-userns.lock"])

    def test_new_install(self):
        self.run_case(success=True)

    def test_identical_file_already_loaded_is_noop(self):
        self.run_case(existing=PROFILE, loaded=True, success=True)

    def test_identical_unloaded_file_loads(self):
        self.run_case(existing=PROFILE, success=True)

    def test_differing_file_refused(self):
        self.run_case(existing="custom policy\n")

    def test_loaded_without_file_refused(self):
        self.run_case(loaded=True)

    def test_parse_failure(self):
        self.run_case(failure="parse")

    def test_new_load_failure_rollback(self):
        self.run_case(failure="load")

    def test_new_verification_failure_rollback(self):
        self.run_case(failure="verify")

    def test_prior_loaded_never_attempts_reload(self):
        self.run_case(existing=PROFILE, loaded=True, failure="load", success=True)

    def test_prior_loaded_never_changes_kernel_label(self):
        self.run_case(existing=PROFILE, loaded=True, failure="verify", success=True)

    def test_prior_unloaded_file_failure_rollback(self):
        self.run_case(existing=PROFILE, failure="load")

    def test_prior_unloaded_file_verification_rollback(self):
        self.run_case(existing=PROFILE, failure="verify")

    def test_rename_failure_restores_absence(self):
        self.run_case(failure="rename")

    def test_signal_after_rename_rolls_back(self):
        self.run_case(failure="signal_after_rename")


if __name__ == "__main__":
    unittest.main()
