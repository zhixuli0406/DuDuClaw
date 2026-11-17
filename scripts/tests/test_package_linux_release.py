"""Exercise release staging, legacy size boundaries and offline signing locally."""
import contextlib
import errno
import hashlib
import importlib.util
import io
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/package-linux-release.py"
SPEC = importlib.util.spec_from_file_location("linux_release", SCRIPT)
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)
TARGET = "x86_64-unknown-linux-gnu"
PIN = "ab" * 32
OTHER_PIN = "cd" * 32
VERSION = PACKAGE.version_text(PACKAGE.workspace_version())
BOOTSTRAP = PACKAGE.BOOTSTRAP_BINARY
# Every variable that could carry a pin or a test-root setting into a test run.
BOOTSTRAP_ENVIRONMENT = (PACKAGE.BOOTSTRAP_PIN_VARIABLE, PACKAGE.BOOTSTRAP_TEST_KEY_VARIABLE, "RUSTFLAGS",
                         "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS",
                         "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS")


def good_probes(pin=PIN, version=VERSION):
    return {"--print-root-pin": (0, f"PRODUCTION {pin}\n".encode()),
            "--version": (0, f"duduclaw-bootstrap {version}\n".encode())}


class LinuxReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binaries = self.root / "bin"
        self.binaries.mkdir()
        for name in PACKAGE.BIN_PACKAGES:
            binary = self.binaries / name
            binary.write_bytes(b"\x7fELF\x00GLIBC_2.17\x00" + name.encode())
            binary.chmod(0o755)
        self.python = self.root / "python"
        self.python.mkdir()
        (self.python / "example.py").write_text("print('fixture')\n")
        self.output = self.root / "out"

    def package(self, target=TARGET, include_bootstrap=True):
        verified = {BOOTSTRAP: PACKAGE.sha256(self.binaries / BOOTSTRAP)} if include_bootstrap else {}
        return PACKAGE.create_archive(self.binaries, self.python, target, self.output,
                                      binaries=PACKAGE.archive_binaries(include_bootstrap), verified=verified)

    def test_both_archives_have_every_binary_regular_executable_and_sdk(self):
        for target in PACKAGE.TARGETS:
            archive = self.package(target)
            with tarfile.open(archive) as bundle:
                for name in [*PACKAGE.BIN_PACKAGES, *PACKAGE.ARCHIVE_ALIASES]:
                    entry = bundle.getmember(name)
                    self.assertTrue(entry.isfile(), name)
                    self.assertEqual(entry.mode, 0o755)
                    source = PACKAGE.ARCHIVE_ALIASES.get(name, name)
                    self.assertEqual(bundle.extractfile(entry).read(), (self.binaries / source).read_bytes())
                self.assertIn("python/example.py", bundle.getnames())
                self.assertFalse(any("fixture" in name for name in bundle.getnames()))
            self.assertEqual(Path(str(archive) + ".sha256").read_text(),
                             f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")

    def test_missing_component_refused_before_archive_output(self):
        for name in PACKAGE.BIN_PACKAGES:
            with self.subTest(name=name):
                binary = self.binaries / name
                content = binary.read_bytes()
                binary.unlink()
                with self.assertRaises(FileNotFoundError):
                    self.package()
                self.assertFalse(self.output.exists())
                binary.write_bytes(content)
                binary.chmod(0o755)

    def test_legacy_exact_size_boundary_and_empty_files(self):
        binary = self.binaries / "duduclaw"
        for size in (0, PACKAGE.MAX_BINARY_BYTES, PACKAGE.MAX_BINARY_BYTES + 1):
            with binary.open("wb") as output:
                output.truncate(size)
            with self.assertRaises(ValueError):
                PACKAGE.validate_binary(binary)
        with binary.open("wb") as output:
            output.truncate(PACKAGE.MAX_BINARY_BYTES - 1)
        PACKAGE.validate_binary(binary)

    def test_symlink_hardlink_and_nonexecutable_refused(self):
        source = self.binaries / "duduclaw"
        linked = self.root / "link"
        linked.symlink_to(source)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(linked)
        linked.unlink()
        os.link(source, linked)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(source)
        linked.unlink()
        source.chmod(0o644)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(source)

    def cargo_layout(self):
        """Mimic Cargo: release/<bin> is a hard link to release/deps/<bin>-<hash>."""
        release = self.root / "cargo" / TARGET / "release"
        (release / "deps").mkdir(parents=True)
        for name in PACKAGE.BIN_PACKAGES:
            os.link(self.binaries / name, release / "deps" / (name + "-0123abcd"))
            os.link(release / "deps" / (name + "-0123abcd"), release / name)
            # Drop the original so only the Cargo pair of links remains.
        for name in PACKAGE.BIN_PACKAGES:
            (self.binaries / name).unlink()
        return release

    def run_main(self, release, calls, extra=(), environment=None, probes=None, host="x86_64",
                 skip_build=True, output=None):
        """Run main() with Cargo, glibc checks and the verifier probe faked.

        `probes` maps a flag to (exit code, stdout); a probe that is not listed
        fails the test, so "never executed" is asserted by omission.
        """
        environment = environment or {}
        probes = probes or {}
        self.build_environment = None

        def fake_run(command, **kwargs):
            calls.append([str(part) for part in command])
            if command[1:2] == ["build"]:
                self.build_environment = kwargs.get("env")
            return subprocess.CompletedProcess(command, 0)

        def fake_probe(path, flag):
            calls.append(["probe", str(path), flag])
            result = probes[flag]
            if isinstance(result, BaseException):
                raise result
            return result(path) if callable(result) else result
        argv = ["package-linux-release.py", "--target", TARGET, "--output-dir", str(self.output),
                *(["--skip-build"] if skip_build else []), *extra]
        with patch.object(PACKAGE, "release_binary_dir", return_value=release), \
                patch.object(PACKAGE.subprocess, "run", fake_run), \
                patch.object(PACKAGE, "probe_bootstrap", fake_probe), \
                patch.object(PACKAGE, "host_architecture", return_value=host), \
                patch.object(sys, "argv", argv), \
                patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": "", **environment}), \
                contextlib.redirect_stdout(output if output is not None else io.StringIO()):
            for variable in BOOTSTRAP_ENVIRONMENT:
                if variable not in environment:
                    os.environ.pop(variable, None)
            PACKAGE.main()

    def archive_names(self):
        with tarfile.open(self.output / "duduclaw-linux-x64.tar.gz") as bundle:
            return set(bundle.getnames())

    def test_hardlinked_cargo_output_packages_via_private_staging_copy(self):
        release = self.cargo_layout()
        self.assertEqual((release / "duduclaw").stat().st_nlink, 2)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(release / "duduclaw")  # direct check is still strict
        calls = []
        self.run_main(release, calls)
        archive = self.output / "duduclaw-linux-x64.tar.gz"
        self.assertTrue(archive.is_file())
        checked = [Path(call[-1]) for call in calls if call[1:2] and call[1].endswith("check_glibc.py")]
        self.assertEqual(len(checked), len(PACKAGE.archive_binaries(False)))
        for path in checked:
            self.assertNotEqual(path.parent, release)
            self.assertEqual(path.parent.name[:len("linux-release-inputs-")], "linux-release-inputs-")
        with tarfile.open(archive) as bundle:
            for name in PACKAGE.archive_binaries(False):
                self.assertEqual(bundle.extractfile(name).read(), (release / name).read_bytes())

    # ---------------------------------------------------------------- bootstrap verifier guard

    def test_no_pin_omits_bootstrap_and_says_so(self):
        # The explicit no-bootstrap mode: no pin, no verifier anywhere in the archive.
        release = self.cargo_layout()
        calls, output = [], io.StringIO()
        self.run_main(release, calls, output=output)
        names = self.archive_names()
        self.assertNotIn(BOOTSTRAP, names)
        self.assertFalse(any(BOOTSTRAP in name for name in names))
        for name in [*PACKAGE.archive_binaries(False), *PACKAGE.ARCHIVE_ALIASES]:
            self.assertIn(name, names)
        lines = output.getvalue().splitlines()
        self.assertIn(PACKAGE.OMITTED_BOOTSTRAP_LINE, lines)
        self.assertIn(f"{BOOTSTRAP} NOT included", lines[-1])
        self.assertFalse(any(call[0] == "probe" for call in calls))
        # Without a pin the build the script runs does not even produce the verifier.
        self.assertNotIn(BOOTSTRAP, PACKAGE.build_command(TARGET, "cargo", False))

    def test_no_pin_omits_bootstrap_even_from_a_test_root_build(self):
        release = self.cargo_layout()
        self.run_main(release, [], environment={"RUSTFLAGS": "--cfg duduclaw_test_root",
                                                PACKAGE.BOOTSTRAP_TEST_KEY_VARIABLE: "RWQtest"})
        self.assertNotIn(BOOTSTRAP, self.archive_names())

    def test_environment_pin_only_feeds_a_build_this_run_performs(self):
        release = self.cargo_layout()
        calls = []
        self.run_main(release, calls, environment={PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN},
                      probes=good_probes(), skip_build=False)
        build = next(call for call in calls if call[1:2] == ["build"])
        self.assertIn(BOOTSTRAP, build)
        self.assertEqual(self.build_environment[PACKAGE.BOOTSTRAP_PIN_VARIABLE], PIN)
        self.assertIn(BOOTSTRAP, self.archive_names())
        # With --skip-build a stale build's environment is not trusted as the expected pin.
        shutil.rmtree(self.output)
        calls = []
        self.run_main(release, calls, environment={PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN}, probes=good_probes())
        self.assertNotIn(BOOTSTRAP, self.archive_names())
        self.assertFalse(any(call[0] == "probe" for call in calls))
        self.assertEqual(PACKAGE.expected_bootstrap_pin(None, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN}, True), PIN)
        self.assertIsNone(PACKAGE.expected_bootstrap_pin(None, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN}, False))
        self.assertIsNone(PACKAGE.expected_bootstrap_pin(None, {}, True))
        self.assertEqual(PACKAGE.expected_bootstrap_pin(OTHER_PIN, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN}, True), OTHER_PIN)

    def test_explicit_pin_is_built_in_and_verified_on_the_host_architecture(self):
        release = self.cargo_layout()
        calls, output = [], io.StringIO()
        self.run_main(release, calls, extra=["--bootstrap-root-sha256", PIN], probes=good_probes(), skip_build=False,
                      output=output)
        self.assertEqual(self.build_environment[PACKAGE.BOOTSTRAP_PIN_VARIABLE], PIN)
        self.assertIn(f"{BOOTSTRAP} included (executed here: PRODUCTION {PIN}, "
                      f"sha256 {PACKAGE.sha256(release / BOOTSTRAP)})", output.getvalue().splitlines()[-1])
        probes = [call for call in calls if call[0] == "probe"]
        self.assertEqual([call[2] for call in probes], ["--print-root-pin", "--version"])
        for call in probes:
            # The probed file is the private snapshot that is archived, not Cargo's output.
            self.assertNotEqual(Path(call[1]).parent, release)
            self.assertEqual(Path(call[1]).name, BOOTSTRAP)
        with tarfile.open(self.output / "duduclaw-linux-x64.tar.gz") as bundle:
            self.assertEqual(bundle.extractfile(BOOTSTRAP).read(), (release / BOOTSTRAP).read_bytes())

    def test_removed_marker_options_are_rejected(self):
        marker = self.root / "marker"
        marker.write_text("anything\n")
        for option in (["--bootstrap-pin-verified-by", str(marker)],
                       ["--emit-bootstrap-pin-marker", str(marker), "--bootstrap-binary", str(marker)],
                       ["--bootstrap-binary", str(marker)]):
            with self.subTest(option=option[0]):
                result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET, "--skip-build",
                                         "--output-dir", str(self.output), "--bootstrap-root-sha256", PIN, *option],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unrecognized arguments", result.stderr)
                self.assertFalse(self.output.exists())
        for name in ("pin_marker_text", "read_pin_marker", "emit_pin_marker", "PIN_MARKER_HEADER"):
            self.assertFalse(hasattr(PACKAGE, name), name)

    def assert_refused(self, release, extra, **kwargs):
        with self.assertRaises((ValueError, SystemExit)):
            self.run_main(release, [], extra=extra, **kwargs)
        self.assertFalse((self.output / "duduclaw-linux-x64.tar.gz").exists())

    def test_unset_test_mismatched_pin_or_version_verifier_is_refused(self):
        release = self.cargo_layout()
        pinned = ["--bootstrap-root-sha256", PIN]
        good = good_probes()
        for name, probes in [
                ("unset", {**good, "--print-root-pin": (1, b"UNSET\n")}),
                ("unset exit 0", {**good, "--print-root-pin": (0, b"UNSET\n")}),
                ("test", {**good, "--print-root-pin": (0, f"TEST {PIN}\n".encode())}),
                ("other pin", {**good, "--print-root-pin": (0, f"PRODUCTION {OTHER_PIN}\n".encode())}),
                ("trailing", {**good, "--print-root-pin": (0, f"PRODUCTION {PIN}\nPRODUCTION {PIN}\n".encode())}),
                ("no newline", {**good, "--print-root-pin": (0, f"PRODUCTION {PIN}".encode())}),
                ("failed exit", {**good, "--print-root-pin": (1, f"PRODUCTION {PIN}\n".encode())}),
                ("other version", good_probes(version="0.0.1")),
                ("version exit", {**good, "--version": (2, good["--version"][1])}),
                ("version missing", {**good, "--version": (2, b"")})]:
            with self.subTest(name=name):
                self.assert_refused(release, pinned, probes=probes)
        # A verifier this host cannot execute fails closed: no attestation fallback,
        # never probed, never packaged, never silently skipped.
        for name, host in [("foreign architecture", "aarch64"), ("non-linux host", None)]:
            with self.subTest(name=name):
                calls = []
                with self.assertRaisesRegex(ValueError, "cannot be executed on this host"):
                    self.run_main(release, calls, extra=pinned, probes=good, host=host)
                self.assertFalse(any(call[0] == "probe" for call in calls))
                self.assertFalse((self.output / "duduclaw-linux-x64.tar.gz").exists())
        for name, error in [("exec format error", OSError(errno.ENOEXEC, "Exec format error")),
                            ("missing loader", FileNotFoundError(errno.ENOENT, "No such file or directory")),
                            ("not executable", PermissionError(errno.EACCES, "Permission denied")),
                            ("timeout", subprocess.TimeoutExpired(BOOTSTRAP, 30))]:
            with self.subTest(name=name):
                with self.assertRaisesRegex(ValueError, "could not be executed"):
                    self.run_main(release, [], extra=pinned, probes={**good, "--print-root-pin": error})
                self.assertFalse((self.output / "duduclaw-linux-x64.tar.gz").exists())
        with self.subTest(name="malformed pin"):
            self.assert_refused(release, ["--bootstrap-root-sha256", PIN.upper()], probes=good)
        # An unpinned build is also refused at the build-only step.
        with self.subTest(name="build-only"):
            with self.assertRaises(ValueError):
                self.run_build_only(release, pinned, {**good, "--print-root-pin": (1, b"UNSET\n")})
        # The archive writer itself only accepts a complete production set.
        with self.assertRaises(ValueError):
            PACKAGE.create_archive(release, self.python, TARGET, self.output, binaries=["duduclaw"])
        with self.assertRaises(ValueError):
            PACKAGE.create_archive(release, self.python, TARGET, self.output,
                                   binaries=[*PACKAGE.BIN_PACKAGES, "duduclaw-isolation-test-signer"])
        self.assertFalse(self.output.exists())

    def run_build_only(self, release, extra, probes):
        def fake_run(command, **kwargs):
            return subprocess.CompletedProcess(command, 0)
        argv = ["package-linux-release.py", "--target", TARGET, "--build-only", *extra]
        with patch.object(PACKAGE, "release_binary_dir", return_value=release), \
                patch.object(PACKAGE.subprocess, "run", fake_run), \
                patch.object(PACKAGE, "probe_bootstrap", lambda path, flag: probes[flag]), \
                patch.object(PACKAGE, "host_architecture", return_value="x86_64"), \
                patch.object(sys, "argv", argv), \
                patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": ""}), \
                contextlib.redirect_stdout(io.StringIO()):
            for variable in BOOTSTRAP_ENVIRONMENT:
                os.environ.pop(variable, None)
            PACKAGE.main()

    def test_build_only_with_pin_verifies_the_fresh_build(self):
        release = self.cargo_layout()
        self.run_build_only(release, ["--bootstrap-root-sha256", PIN], good_probes())
        with self.assertRaises(ValueError):
            self.run_build_only(release, ["--bootstrap-root-sha256", PIN],
                                {**good_probes(), "--print-root-pin": (0, f"TEST {PIN}\n".encode())})

    def test_test_root_build_settings_are_refused_with_a_pin(self):
        refused = [{"RUSTFLAGS": "-C opt-level=s --cfg duduclaw_test_root"},
                   {"RUSTFLAGS": "--cfg=duduclaw_test_root"},
                   {"RUSTFLAGS": "--cfg  duduclaw_test_root=\"1\""},
                   {"RUSTFLAGS": "duduclaw_test_root"},
                   {"CARGO_ENCODED_RUSTFLAGS": "-Copt-level=s\x1f--cfg\x1fduduclaw_test_root"},
                   {"CARGO_BUILD_RUSTFLAGS": "--cfg 'duduclaw_test_root'"},
                   {"CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS": "--cfg duduclaw_test_root"},
                   {PACKAGE.BOOTSTRAP_TEST_KEY_VARIABLE: "RWQtest"},
                   {PACKAGE.BOOTSTRAP_TEST_KEY_VARIABLE: ""}]
        for environment in refused:
            with self.subTest(environment=environment):
                with self.assertRaises(ValueError):
                    PACKAGE.refuse_test_root_environment(environment, TARGET)
        # Tokenised flags, not substring matching: unrelated cfgs and names pass.
        for environment in [{}, {"RUSTFLAGS": "--cfg duduclaw_test_root_extra"},
                            {"RUSTFLAGS": "--cfg=feature=\"duduclaw_test_root\""},
                            {"RUSTFLAGS": "-Cmetadata=duduclaw_test_root"},
                            {"CARGO_ENCODED_RUSTFLAGS": "--cfg\x1fother"},
                            {"CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS": "--cfg duduclaw_test_root"}]:
            with self.subTest(environment=environment):
                PACKAGE.refuse_test_root_environment(environment, TARGET)
        # Wired into main(): refused before anything is packaged, even if the binary would pass.
        release = self.cargo_layout()
        for environment in refused[:3] + refused[-1:]:
            with self.subTest(main=environment):
                self.assert_refused(release, ["--bootstrap-root-sha256", PIN], probes=good_probes(),
                                    environment=environment)

    def test_pin_format_is_strict(self):
        PACKAGE.parse_bootstrap_pin(PIN)
        for bad in [PIN.upper(), "ab" * 31, "ab" * 33, PIN + "\n", " " + PIN, "zz" * 32, "", None]:
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError):
                    PACKAGE.parse_bootstrap_pin(bad)
        with self.assertRaises(ValueError):
            PACKAGE.expected_bootstrap_pin(None, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: "not-a-pin"}, True)

    def test_wrong_format_binary_is_refused_by_the_real_probe(self):
        # Unpatched probe: a file the kernel cannot execute is a refusal, not a skip.
        binary = self.binaries / BOOTSTRAP
        binary.write_bytes(b"\x7fELF\x00not-a-loadable-image")
        binary.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "could not be executed"):
            PACKAGE.check_probe(binary, "--print-root-pin", f"PRODUCTION {PIN}\n")
        with patch.object(PACKAGE, "host_architecture", return_value="x86_64"):
            with self.assertRaisesRegex(ValueError, "could not be executed"):
                PACKAGE.verify_bootstrap_binary(binary, TARGET, PIN, VERSION)
        binary.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "could not be executed"):
            PACKAGE.check_probe(binary, "--print-root-pin", f"PRODUCTION {PIN}\n")

    def test_nonexecutable_bootstrap_is_refused_when_requested(self):
        release = self.cargo_layout()
        (release / BOOTSTRAP).chmod(0o644)
        self.assert_refused(release, ["--bootstrap-root-sha256", PIN], probes=good_probes())

    def test_bytes_changed_while_probing_are_refused(self):
        release = self.cargo_layout()

        def rewrite(path):
            Path(path).write_bytes(b"\x7fELF\x00GLIBC_2.17\x00swapped-after-pin-probe")
            return good_probes()["--version"]
        self.assert_refused(release, ["--bootstrap-root-sha256", PIN],
                            probes={**good_probes(), "--version": rewrite})

    def test_bytes_changed_between_probe_and_archive_are_refused(self):
        release = self.cargo_layout()

        def tamper(binary_dir, *args, **kwargs):
            (binary_dir / BOOTSTRAP).write_bytes(b"\x7fELF\x00GLIBC_2.17\x00test-root-build")
            return []
        with patch.object(PACKAGE, "sign_release", tamper):
            with self.assertRaisesRegex(ValueError, "changed after it was verified"):
                self.run_main(release, [], extra=["--bootstrap-root-sha256", PIN], probes=good_probes())
        self.assertFalse((self.output / "duduclaw-linux-x64.tar.gz").exists())

    def test_archive_writer_requires_the_probed_digest_for_the_verifier(self):
        everything = PACKAGE.archive_binaries(True)
        for name, verified in [("missing", {}), ("not a string", {BOOTSTRAP: None}),
                               ("other bytes", {BOOTSTRAP: "00" * 32}),
                               ("unknown name", {BOOTSTRAP: PACKAGE.sha256(self.binaries / BOOTSTRAP),
                                                 "duduclaw-isolation-test-signer": "00" * 32})]:
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output,
                                           binaries=everything, verified=verified)
                self.assertFalse((self.output / "duduclaw-linux-x64.tar.gz").exists())
        # A digest for a verifier that is not being archived is refused too.
        with self.assertRaises(ValueError):
            PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output,
                                   binaries=PACKAGE.archive_binaries(False),
                                   verified={BOOTSTRAP: PACKAGE.sha256(self.binaries / BOOTSTRAP)})
        self.assertFalse((self.output / "duduclaw-linux-x64.tar.gz").exists())

    def test_staged_copy_is_independent_private_nlink_one(self):
        release = self.cargo_layout()
        staging = Path(tempfile.mkdtemp(dir=self.root))
        staging.chmod(0o700)
        staged = staging / "duduclaw"
        PACKAGE.stage_cargo_binary(release / "duduclaw", staged)
        metadata = staged.lstat()
        self.assertEqual(metadata.st_nlink, 1)
        self.assertEqual(metadata.st_mode & 0o7777, 0o755)
        self.assertNotEqual(metadata.st_ino, (release / "duduclaw").stat().st_ino)
        before = staged.read_bytes()
        (release / "duduclaw").write_bytes(b"\x7fELFmutated")  # writes through the Cargo links
        self.assertEqual(staged.read_bytes(), before)

    def test_staging_refuses_symlinked_and_nonregular_inputs(self):
        source = self.binaries / "duduclaw"
        linked = self.root / "link"
        linked.symlink_to(source)
        staging = Path(tempfile.mkdtemp(dir=self.root))
        with self.assertRaises(ValueError):
            PACKAGE.stage_cargo_binary(linked, staging / "duduclaw")
        self.assertFalse((staging / "duduclaw").exists())
        with self.assertRaises(ValueError):
            PACKAGE.stage_cargo_binary(self.binaries, staging / "dir")
        source.chmod(0o644)
        with self.assertRaises(ValueError):
            PACKAGE.stage_cargo_binary(source, staging / "duduclaw")

    def test_symlinked_cargo_output_refused_by_main(self):
        release = self.cargo_layout()
        target = self.root / "elsewhere"
        shutil.copyfile(release / "duduclaw", target)
        target.chmod(0o755)
        (release / "duduclaw").unlink()
        (release / "duduclaw").symlink_to(target)
        with self.assertRaises(ValueError):
            self.run_main(release, [])
        self.assertFalse(self.output.exists())

    def test_output_is_not_overwritten(self):
        archive = self.package()
        before = archive.read_bytes()
        with self.assertRaises(ValueError):
            self.package()
        self.assertEqual(before, archive.read_bytes())

    def test_explicit_production_build_includes_parser_engine(self):
        command = PACKAGE.build_command(TARGET, "cross", True)
        self.assertEqual(command[:2], ["cross", "build"])
        self.assertIn("--locked", command)
        self.assertNotIn("--bins", command)
        self.assertNotIn("duduclaw-run-root-fixture", command)
        for name in PACKAGE.BIN_PACKAGES:
            self.assertIn(name, [command[i + 1] for i, part in enumerate(command) if part == "--bin"])
        self.assertIn("duduclaw-parse-worker/engine", command[command.index("--features") + 1])
        unpinned = PACKAGE.build_command(TARGET, "cross", False)
        bins = [unpinned[i + 1] for i, part in enumerate(unpinned) if part == "--bin"]
        self.assertEqual(bins, PACKAGE.archive_binaries(False))
        self.assertNotIn(BOOTSTRAP, bins)

    def test_f_publication_names_and_packaged_component_ids_agree(self):
        source = (ROOT / "crates/duduclaw-updated/src/publication.rs").read_text()
        variants = {"gateway": "Gateway", "spawnd": "Spawnd", "trampoline": "Trampoline",
            "egress": "Egress", "egress_forwarder": "EgressForwarder", "mcp_bridge": "McpBridge",
            "parse_worker": "ParseWorker", "importer": "Importer", "migration": "Migration", "update_slot": "UpdateSlot"}
        for kind, variant in variants.items():
            match = re.search(r'InstallTarget::' + variant + r' => "([^"]+)"', source)
            self.assertIsNotNone(match, kind)
            filename = match[1]
            self.assertEqual(PACKAGE.ARCHIVE_ALIASES.get(filename, filename), PACKAGE.INSTALL_BINARIES[kind])
        self.assertEqual(PACKAGE.ARCHIVE_ALIASES["duduclaw-updated-launch"], "duduclaw-updated")

    def test_relative_target_dir_matches_cargo_working_directory(self):
        original = Path.cwd()
        try:
            os.chdir(self.root)
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": "build"}):
                self.assertEqual(PACKAGE.release_binary_dir(TARGET), ROOT / "build" / TARGET / "release")
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.binaries)}):
                self.assertEqual(PACKAGE.release_binary_dir(TARGET), self.binaries / TARGET / "release")
        finally:
            os.chdir(original)

    def test_absent_offline_key_is_explicit_skip(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(PACKAGE.sign_release(self.binaries, TARGET, self.output,
                             None, None, "ssh-keygen"), [])
        self.assertIn("SKIP protected release manifest", output.getvalue())
        self.assertFalse(self.output.exists())

    def test_ci_refuses_signing_before_reading_key(self):
        with patch.dict(os.environ, {"CI": "true"}):
            with self.assertRaisesRegex(ValueError, "never be used in CI"):
                PACKAGE.sign_release(self.binaries, TARGET, self.output,
                                     self.root / "missing", self.root / "key", "ssh-keygen")

    def bootstrap_inputs(self):
        self.output.mkdir()
        manifest = self.manifest()
        for artifact in manifest["artifacts"]:
            content = (self.binaries / PACKAGE.INSTALL_BINARIES[artifact["install_target"]["kind"]]).read_bytes()
            (self.output / ("isolated-linux-x86_64-" + artifact["id"])).write_bytes(content)
            artifact["sha256"] = list(hashlib.sha256(content).digest())
            artifact["size_bytes"] = len(content)
        canonical = lambda value: json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
        (self.output / "isolated-linux-x86_64-manifest.json").write_bytes(canonical(manifest))
        (self.output / "isolated-linux-x86_64-manifest.json.signatures.json").write_text('["target-fixture"]')
        root = self.root / "root.json"
        root.write_bytes(canonical({"schema_version": 1, "version": 1}))
        signatures = self.root / "root.signatures.json"
        signatures.write_text('["root-fixture-a","root-fixture-b"]')
        return root, signatures

    def test_bootstrap_inbox_matches_verifier_layout_without_active_state(self):
        root, signatures = self.bootstrap_inputs()
        inbox = PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        self.assertEqual({path.name for path in inbox.iterdir()}, {
            "root.json", "root.json.signatures.json", "manifest.json", "manifest.json.signatures.json", "artifacts"})
        self.assertEqual((inbox / "root.json").read_bytes(), root.read_bytes())
        self.assertEqual(len(list((inbox / "artifacts").iterdir())), len(PACKAGE.REQUIRED_TARGETS))
        for path in (inbox / "artifacts").iterdir():
            self.assertEqual(path.stat().st_mode & 0o7777, 0o600)
        with self.assertRaisesRegex(ValueError, "already exists"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)

    def test_bootstrap_inbox_refuses_changed_signed_bytes_before_publication(self):
        root, signatures = self.bootstrap_inputs()
        (self.output / "isolated-linux-x86_64-gateway").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "changed"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        self.assertFalse((self.output / "isolated-linux-x86_64-bootstrap-inbox").exists())

    def test_bootstrap_inbox_refuses_missing_root_threshold_and_incomplete_generation(self):
        root, signatures = self.bootstrap_inputs()
        signatures.write_text('["one-key"]')
        with self.assertRaisesRegex(ValueError, "threshold"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        signatures.write_text('["first","second"]')
        path = self.output / "isolated-linux-x86_64-manifest.json"
        manifest = json.loads(path.read_text())
        manifest["artifacts"] = [a for a in manifest["artifacts"] if a["install_target"]["kind"] != "migration"]
        path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")))
        with self.assertRaisesRegex(ValueError, "incomplete"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)

    def test_bootstrap_flags_require_offline_signer_before_build(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET,
            "--bootstrap-root", str(self.root / "missing"), "--skip-build"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("bootstrap inbox requires", result.stderr)

    def manifest(self):
        return {"schema_version": 1, "product": "duduclaw", "channel": "stable",
                "os": "linux", "arch": "x86_64",
                "version": PACKAGE.workspace_version(),
                "minimum_version": {"major": 1, "minor": 79, "patch": 3},
                "serial": 1, "created_at_unix": 1793808000, "expires_at_unix": 1893456000,
                "protocols": {name: 1 for name in ("gateway", "spawnd", "trampoline", "egress",
                              "updated", "mcp_bridge", "parse_worker", "importer")},
                "migrations": [], "artifacts": [{"id": name, "mode": 0o755,
                "install_target": {"kind": kind}} for kind, name in
                ((kind, kind) for kind in PACKAGE.INSTALL_BINARIES)]}

    def test_signed_runtime_targets_are_distinct_and_have_fixed_sources_and_modes(self):
        document = self.manifest()
        for registry, mode in [("isolation-policy-v1", 0o444), ("api-worker-v1", 0o755),
                               ("native-adapter-v1", 0o755), ("vendor-codex", 0o755), ("vendor-grok", 0o755)]:
            document["artifacts"].append({"id": registry, "mode": mode,
                "install_target": {"kind": "runtime_entry", "registry_id": registry}})
        PACKAGE.validate_targets(document["artifacts"])
        self.assertEqual(PACKAGE.runtime_source(document["artifacts"][11], self.binaries, None),
                         self.binaries / "duduclaw-api-worker")
        with self.assertRaises(ValueError):
            PACKAGE.runtime_source(document["artifacts"][10], self.binaries, None)
        self.assertEqual(PACKAGE.runtime_source(document["artifacts"][10], self.binaries, self.root),
                         self.root / "runtime-isolation-policy-v1")
        duplicate = document["artifacts"] + [document["artifacts"][-1]]
        with self.assertRaises(ValueError):
            PACKAGE.validate_targets(duplicate)
        for registry, mode in [("../vendor", 0o755), ("vendor-grok", 0o444), ("isolation-policy-v1", 0o755)]:
            bad = {"mode": mode, "install_target": {"kind": "runtime_entry", "registry_id": registry}}
            with self.assertRaises(ValueError):
                PACKAGE.runtime_target(bad)

    def test_wrong_arch_product_version_and_incomplete_template_refused(self):
        template = self.root / "template.json"
        for field, value in (("arch", "aarch64"), ("product", "duduclaw_pro"),
                             ("version", {"major": 0, "minor": 0, "patch": 0}), ("artifacts", [])):
            document = self.manifest()
            document[field] = value
            template.write_text(json.dumps(document))
            with patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": ""}):
                with self.assertRaises(ValueError):
                    PACKAGE.sign_release(self.binaries, TARGET, self.output, template,
                                         self.root / "key", "ssh-keygen")

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires OpenSSH")
    def test_offline_signer_generates_matching_assets_and_verified_signature(self):
        key = self.root / "test-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        template = self.root / "template.json"
        document = self.manifest()
        # Valid IDs can collide with another ID's publication prefix. Raw
        # artifacts must stay intact while publication names are constructed.
        document["artifacts"][1]["id"] = "isolated-linux-x86_64-gateway"
        template.write_text(json.dumps(document))
        with patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": ""}):
            outputs = PACKAGE.sign_release(self.binaries, TARGET, self.output, template, key, "ssh-keygen")
        self.assertEqual(len(outputs), 12)
        manifest = self.output / "isolated-linux-x86_64-manifest.json"
        document = json.loads(manifest.read_bytes())
        for artifact in document["artifacts"]:
            payload = (self.output / ("isolated-linux-x86_64-" + artifact["id"])).read_bytes()
            self.assertEqual(artifact["sha256"], list(hashlib.sha256(payload).digest()))
            self.assertEqual(artifact["size_bytes"], len(payload))
        signatures = json.loads(Path(str(manifest) + ".signatures.json").read_text())
        detached = self.root / "signature"
        detached.write_text(signatures[0])
        allowed = self.root / "allowed"
        allowed.write_text("release " + Path(str(key) + ".pub").read_text())
        result = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed), "-I", "release",
            "-n", "duduclaw-release-manifest-v1@dudustudio", "-s", str(detached)],
            input=manifest.read_bytes(), capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires OpenSSH")
    def test_signed_runtime_policy_and_worker_publish_through_bootstrap_inbox(self):
        key = self.root / "runtime-test-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        runtime_dir = self.root / "runtime"
        runtime_dir.mkdir()
        (runtime_dir / "runtime-isolation-policy-v1").write_text(
            '{"domain":"duduclaw-isolation-policy-v2","version":2,"runtimes":[],"native":[]}')
        (runtime_dir / "runtime-vendor-codex").write_bytes(b"\x7fELF-test-only-vendor")
        (runtime_dir / "runtime-vendor-codex").chmod(0o755)
        document = self.manifest()
        for registry, mode in [("isolation-policy-v1", 0o444), ("api-worker-v1", 0o755),
                               ("native-adapter-v1", 0o755), ("vendor-codex", 0o755)]:
            document["artifacts"].append({"id":registry,"mode":mode,
                "install_target":{"kind":"runtime_entry","registry_id":registry}})
        template = self.root / "runtime-template.json"
        template.write_text(json.dumps(document))
        with patch.dict(os.environ, {"CI":"", "GITHUB_ACTIONS":""}):
            outputs = PACKAGE.sign_release(self.binaries,TARGET,self.output,template,key,"ssh-keygen",runtime_dir)
        self.assertEqual(len(outputs),16)
        canonical = lambda value: json.dumps(value,sort_keys=True,separators=(",", ":")).encode()
        root = self.root / "root.json"
        root.write_bytes(canonical({"schema_version":1,"version":1}))
        signatures = self.root / "root.signatures.json"
        signatures.write_text('["root-fixture-a","root-fixture-b"]')
        inbox = PACKAGE.create_bootstrap_inbox(TARGET,self.output,root,signatures)
        manifest = json.loads((inbox / "manifest.json").read_bytes())
        self.assertEqual(len(list((inbox / "artifacts").iterdir())),14)
        for artifact in manifest["artifacts"]:
            payload = (inbox / "artifacts" / artifact["id"]).read_bytes()
            self.assertEqual(artifact["sha256"],list(hashlib.sha256(payload).digest()))
            self.assertEqual(artifact["size_bytes"],len(payload))

    def test_release_entrypoint_and_workflow_share_helper(self):
        result = subprocess.run(["bash", str(ROOT / "scripts/release.sh"), "linux-assets", "--help"],
                                cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--signing-key", result.stdout)
        for workflow in ("release.yml", "release-cache-warm.yml"):
            body = (ROOT / ".github/workflows" / workflow).read_text()
            self.assertIn("package-linux-release.py --target ${{ matrix.target }} --build-only", body)


if __name__ == "__main__":
    unittest.main()
