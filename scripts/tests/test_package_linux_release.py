"""Exercise release staging, legacy size boundaries and offline signing locally."""
import contextlib
import errno
import fcntl
import hashlib
import importlib.util
import inspect
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


LINUX = sys.platform.startswith("linux")


def good_probes(pin=PIN, version=VERSION):
    return {"--print-root-pin": (0, f"PRODUCTION {pin}\n".encode()),
            "--version": (0, f"duduclaw-bootstrap {version}\n".encode())}


def elf_image(target, tag=b"verifier"):
    """A verifier fixture whose ELF header names `target` (the sealed flow checks it)."""
    machine = PACKAGE.ELF_MACHINES[PACKAGE.TARGETS[target][0]]
    header = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9) + (2).to_bytes(2, "little") + machine.to_bytes(2, "little")
    return header + b"\x00GLIBC_2.17\x00" + tag


class StandInSeals:
    """TEST-ONLY stand-in for _new_image/_add_seals/_get_seals where memfd does not
    exist (macOS): an unlinked private file whose inode is recorded as "sealed".
    It is installed only by patching (see LinuxReleaseTests.seam); the script has no
    option that selects it. Linux runs use the real sealed memfd."""

    def __init__(self):
        self.sealed = set()

    @staticmethod
    def identity(descriptor):
        metadata = os.fstat(descriptor)
        return metadata.st_dev, metadata.st_ino

    def new_image(self):
        descriptor, name = tempfile.mkstemp()
        os.unlink(name)
        return descriptor

    def add_seals(self, descriptor):
        self.sealed.add(self.identity(descriptor))

    def get_seals(self, descriptor):
        return PACKAGE.BOOTSTRAP_SEALS if self.identity(descriptor) in self.sealed else 0

    def patches(self):
        return [patch.object(PACKAGE, "_new_image", self.new_image),
                patch.object(PACKAGE, "_add_seals", self.add_seals),
                patch.object(PACKAGE, "_get_seals", self.get_seals)]


def unsealed_copy(descriptor):
    """A new descriptor with the same bytes and NO seals (real memfd on Linux)."""
    content = image_bytes(descriptor)
    if LINUX:
        copy = os.memfd_create("unsealed-substitute", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
    else:
        copy, name = tempfile.mkstemp()
        os.unlink(name)
    os.write(copy, content)
    return copy


def image_bytes(descriptor):
    return os.pread(descriptor, os.fstat(descriptor).st_size, 0)


def answer(probes, descriptor, flag):
    result = probes[flag]
    if isinstance(result, BaseException):
        raise result
    return result(descriptor) if callable(result) else result


def replace_file(path, content):
    """Atomically swap the pathname to another inode (what a same-uid attacker can do)."""
    swapped = Path(str(path) + ".swap")
    swapped.write_bytes(content)
    swapped.chmod(0o755)
    os.replace(swapped, path)


class LinuxReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binaries = self.root / "bin"
        self.binaries.mkdir()
        for name in PACKAGE.BIN_PACKAGES:
            binary = self.binaries / name
            binary.write_bytes(elf_image(TARGET) if name == BOOTSTRAP
                               else b"\x7fELF\x00GLIBC_2.17\x00" + name.encode())
            binary.chmod(0o755)
        self.python = self.root / "python"
        self.python.mkdir()
        (self.python / "example.py").write_text("print('fixture')\n")
        self.output = self.root / "out"

    @contextlib.contextmanager
    def seam(self, probes=None, host="x86_64", calls=None):
        """Run the real sealed flow with only the probe execution (and, off Linux, the
        memfd primitives) faked. Each source read is recorded as ["read", path]
        and each probe as ["probe", sha256 of the image it was given, flag]; a
        probe that is not listed fails the test, so "never executed" is asserted
        by omission."""
        probes = good_probes() if probes is None else probes
        calls = [] if calls is None else calls
        path_source = PACKAGE._path_source

        def fake_probe(descriptor, flag):
            calls.append(["probe", hashlib.sha256(image_bytes(descriptor)).hexdigest(), flag])
            return answer(probes, descriptor, flag)

        def recording_source(path):
            calls.append(["read", str(path)])
            return path_source(path)
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(PACKAGE, "host_architecture", return_value=host))
            stack.enter_context(patch.object(PACKAGE, "_probe_image", fake_probe))
            stack.enter_context(patch.object(PACKAGE, "_path_source", recording_source))
            if not LINUX:
                for stand_in in StandInSeals().patches():
                    stack.enter_context(stand_in)
            yield calls

    def create(self, target=TARGET, pin=PIN, output=None, probes=None, host="native", calls=None, **kwargs):
        """create_archive with the sealed flow (pin) or without the verifier (pin=None);
        `host` defaults to the target's own architecture."""
        expectations = {"bootstrap_pin": pin, "bootstrap_version": VERSION} if pin is not None else {}
        host = PACKAGE.TARGETS[target][0] if host == "native" else host
        with self.seam(probes, host, calls):
            return PACKAGE.create_archive(self.binaries, self.python, target, output or self.output,
                                          binaries=PACKAGE.archive_binaries(False), **expectations, **kwargs)

    def package(self, target=TARGET, include_bootstrap=True, output=None):
        archive, report = self.create(target, PIN if include_bootstrap else None, output)
        self.assertEqual(report is None, not include_bootstrap)
        return archive

    def test_both_archives_have_every_binary_regular_executable_and_sdk(self):
        for target in PACKAGE.TARGETS:
            (self.binaries / BOOTSTRAP).write_bytes(elf_image(target))
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
        """Run main() with Cargo and glibc checks faked and the sealed flow under self.seam().

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
        argv = ["package-linux-release.py", "--target", TARGET, "--output-dir", str(self.output),
                *(["--skip-build"] if skip_build else []), *extra]
        with patch.object(PACKAGE, "release_binary_dir", return_value=release), \
                patch.object(PACKAGE.subprocess, "run", fake_run), \
                self.seam(probes, host, calls), \
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
        glibc = [call for call in calls if any(str(arg).endswith("check_glibc.py") for arg in call[1:4])]
        # Run in isolated mode without bytecode: no PYTHONPATH, user site or script-directory import.
        self.assertTrue(glibc and all(call[1:3] == ["-I", "-B"] for call in glibc))
        checked = [Path(call[-1]) for call in glibc]
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
        # Both probes executed the one sealed image of the built bytes ...
        self.assertEqual({call[1] for call in probes}, {PACKAGE.sha256(release / BOOTSTRAP)})
        # ... which was read exactly once, from the private snapshot, not Cargo's output.
        reads = [call for call in calls if call[0] == "read"]
        self.assertEqual(len(reads), 1)
        self.assertNotEqual(Path(reads[0][1]).parent, release)
        self.assertEqual(Path(reads[0][1]).name, BOOTSTRAP)
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
                # Refused before the verifier is even read, let alone executed.
                self.assertFalse(any(call[0] in ("probe", "read") for call in calls))
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

    def run_build_only(self, release, extra, probes, calls=None):
        def fake_run(command, **kwargs):
            return subprocess.CompletedProcess(command, 0)
        argv = ["package-linux-release.py", "--target", TARGET, "--build-only", "--output-dir", str(self.output),
                *extra]
        with patch.object(PACKAGE, "release_binary_dir", return_value=release), \
                patch.object(PACKAGE.subprocess, "run", fake_run), \
                self.seam(probes, "x86_64", calls), \
                patch.object(sys, "argv", argv), \
                patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": ""}), \
                contextlib.redirect_stdout(io.StringIO()):
            for variable in BOOTSTRAP_ENVIRONMENT:
                os.environ.pop(variable, None)
            PACKAGE.main()

    def test_build_only_with_pin_verifies_the_fresh_build(self):
        release = self.cargo_layout()
        calls = []
        self.run_build_only(release, ["--bootstrap-root-sha256", PIN], good_probes(), calls)
        # Sealed straight from Cargo's output (one read), executed, nothing written.
        self.assertEqual([call[1] for call in calls if call[0] == "read"], [str(release / BOOTSTRAP)])
        self.assertEqual([call[2] for call in calls if call[0] == "probe"], ["--print-root-pin", "--version"])
        self.assertFalse(self.output.exists())
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

    def real_host_target(self):
        arch = PACKAGE.host_architecture()
        target = next((name for name, (machine, _) in PACKAGE.TARGETS.items() if machine == arch), None)
        if target is None:
            self.skipTest("not a native x86_64/aarch64 Linux host")
        return target

    @unittest.skipUnless(LINUX, "the real probe executes a sealed memfd (Linux only)")
    def test_wrong_format_binary_is_refused_by_the_real_probe(self):
        # Unpatched seal and probe: an image the kernel cannot execute (a header for
        # this architecture, nothing loadable behind it) is a refusal, not a skip.
        target = self.real_host_target()
        binary = self.binaries / BOOTSTRAP
        binary.write_bytes(elf_image(target, b"not-a-loadable-image"))
        binary.chmod(0o755)
        with self.assertRaisesRegex(ValueError, "could not be executed"):
            PACKAGE.check_bootstrap_file(binary, target, PIN, VERSION)

    @unittest.skipIf(LINUX, "covers the non-Linux refusal of the real code path")
    def test_real_verifier_path_is_refused_off_linux(self):
        # Unpatched: no native Linux host, and no memfd to seal into either.
        with self.assertRaisesRegex(ValueError, "cannot be executed on this host"):
            PACKAGE.check_bootstrap_file(self.binaries / BOOTSTRAP, TARGET, PIN, VERSION)
        with self.assertRaisesRegex(ValueError, "cannot be executed on this host"):
            PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output,
                                   binaries=PACKAGE.archive_binaries(False), bootstrap_pin=PIN,
                                   bootstrap_version=VERSION)
        self.assertFalse(self.output.exists())
        with self.assertRaisesRegex(ValueError, "only be sealed for probing on Linux"):
            PACKAGE._new_image()
        # Nothing reads as sealed here, so even a bypassed host check cannot pass the flow.
        with open(self.binaries / BOOTSTRAP, "rb") as handle:
            self.assertEqual(PACKAGE._get_seals(handle.fileno()), 0)
        with patch.object(PACKAGE, "host_architecture", return_value="x86_64"):
            with self.assertRaisesRegex(ValueError, "only be sealed for probing on Linux"):
                PACKAGE.check_bootstrap_file(self.binaries / BOOTSTRAP, TARGET, PIN, VERSION)
        # The stand-in seam is test-only: the CLI never reaches it. An archive made
        # with the seam is still refused by the real script on this host.
        shipped = self.archive_with(True, "seam-made")
        result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET, "--verify-archive", str(shipped),
                                 "--bootstrap-root-sha256", PIN], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot be executed on this host", result.stderr)

    @unittest.skipUnless(LINUX and os.path.exists("/bin/sh"), "executes a sealed memfd script (Linux only)")
    def test_real_probe_executes_the_sealed_image_in_a_clean_child(self):
        # The probe child gets no environment, /dev/null as stdin and an empty working
        # directory; only builtins are used so no PATH is needed.
        script = self.binaries / BOOTSTRAP

        def write_script(body):
            script.write_text("#!/bin/sh\n"
                              '[ -z "${HOME+x}${PATH_LEAK+x}" ] || exit 3\n'
                              'for entry in * .[!.]* ..?*; do [ -e "$entry" ] && exit 4; done\n'
                              "read -r line && exit 5\n" + body)
            script.chmod(0o755)
        write_script('case "$1" in\n'
                     f"--print-root-pin) printf 'PRODUCTION %s\\n' {PIN} ;;\n"
                     f"--version) printf 'duduclaw-bootstrap %s\\n' {VERSION} ;;\n"
                     "*) exit 6 ;;\nesac\n")
        target = self.real_host_target()
        # A shell script is not an ELF; only the ELF-target check is bypassed here so
        # the real sealing and the real probe child can be observed.
        no_elf_check = patch.object(PACKAGE, "_require_target_elf", lambda descriptor, arch: None)
        with no_elf_check, patch.dict(os.environ, {"PATH_LEAK": "1", "HOME": "/nonexistent"}):
            archive, report = PACKAGE.create_archive(self.binaries, self.python, target, self.output,
                                                     binaries=PACKAGE.archive_binaries(False),
                                                     bootstrap_pin=PIN, bootstrap_version=VERSION)
        self.assertEqual((report.target, report.sha256, report.pin, report.version),
                         (target, PACKAGE.sha256(script), PIN, VERSION))
        self.assertEqual(archive.name, PACKAGE.TARGETS[target][1] + ".tar.gz")
        with tarfile.open(archive) as bundle:
            self.assertEqual(bundle.extractfile(BOOTSTRAP).read(), script.read_bytes())
        # Output beyond the bound is refused, not truncated into a match.
        write_script(f"printf 'PRODUCTION %s\\n' {PIN}\n" + "printf '%0300d\\n' 0\n")
        with no_elf_check, self.assertRaisesRegex(ValueError, "did not print exactly"):
            PACKAGE.check_bootstrap_file(script, target, PIN, VERSION)
        # Without the bypass the script is refused before it is executed.
        with self.assertRaisesRegex(ValueError, "not a 64-bit little-endian ELF"):
            PACKAGE.check_bootstrap_file(script, target, PIN, VERSION)

    @unittest.skipUnless(LINUX, "memfd seals are Linux-only")
    def test_sealed_image_cannot_be_written_resized_or_resealed(self):
        source = self.binaries / BOOTSTRAP
        content = source.read_bytes()
        with PACKAGE._SealedImage(lambda: PACKAGE._path_source(source)) as image:
            descriptor = image.descriptor
            self.assertEqual(image_bytes(descriptor), content)
            self.assertEqual(PACKAGE._get_seals(descriptor), PACKAGE.BOOTSTRAP_SEALS)
            self.assertEqual(fcntl.fcntl(descriptor, PACKAGE.F_GET_SEALS), PACKAGE.BOOTSTRAP_SEALS)
            for attempt in (lambda: os.pwrite(descriptor, b"x", 0), lambda: os.write(descriptor, b"x"),
                            lambda: os.ftruncate(descriptor, 0),
                            lambda: os.ftruncate(descriptor, len(content) + 4096),
                            lambda: fcntl.fcntl(descriptor, PACKAGE.F_ADD_SEALS, PACKAGE.F_SEAL_SEAL)):
                with self.assertRaises(PermissionError):
                    attempt()
            self.assertEqual(image_bytes(descriptor), content)
            # Close-on-exec in this process; only the probe child gets it (pass_fds), never fd 0-2.
            self.assertFalse(os.get_inheritable(descriptor))
            self.assertGreaterEqual(descriptor, 3)
        # Closed when the block ends; nothing keeps the descriptor.
        self.assertIsNone(image.descriptor)
        with self.assertRaises(OSError):
            os.fstat(descriptor)
        # An unsealed memfd reads back as such.
        unsealed = os.memfd_create("unsealed", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
        self.addCleanup(os.close, unsealed)
        self.assertNotEqual(PACKAGE._get_seals(unsealed), PACKAGE.BOOTSTRAP_SEALS)
        # The source must be an executable regular file, never followed through a symlink.
        linked = self.root / "linked-bootstrap"
        linked.symlink_to(source)
        with self.assertRaisesRegex(ValueError, "symbolic link"):
            with PACKAGE._SealedImage(lambda: PACKAGE._path_source(linked)):
                pass
        source.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "executable regular file"):
            with PACKAGE._SealedImage(lambda: PACKAGE._path_source(source)):
                pass

    def test_nonexecutable_bootstrap_is_refused_when_requested(self):
        release = self.cargo_layout()
        (release / BOOTSTRAP).chmod(0o644)
        self.assert_refused(release, ["--bootstrap-root-sha256", PIN], probes=good_probes())

    def test_path_swapped_after_the_read_is_irrelevant(self):
        # A same-uid process swaps the pathname during each probe: the archive holds
        # the bytes that were read once, sealed and executed.
        path = self.binaries / BOOTSTRAP
        original = path.read_bytes()

        def swapping(flag):
            def probe(descriptor):
                self.assertEqual(image_bytes(descriptor), original)
                replace_file(path, elf_image(TARGET, b"swapped-in-during" + flag.encode()))
                return good_probes()[flag]
            return probe
        archive, report = self.create(probes={flag: swapping(flag) for flag in good_probes()})
        self.assertNotEqual(path.read_bytes(), original)
        self.assertEqual((report.sha256, report.size), (hashlib.sha256(original).hexdigest(), len(original)))
        with tarfile.open(archive) as bundle:
            member = bundle.getmember(BOOTSTRAP)
            self.assertTrue(member.isfile())
            self.assertEqual(member.mode, 0o755)
            self.assertEqual(bundle.extractfile(member).read(), original)

    def test_snapshot_swapped_during_the_probe_is_irrelevant_in_main(self):
        release = self.cargo_layout()
        original = (release / BOOTSTRAP).read_bytes()
        calls = []

        def swap(descriptor):
            # The snapshot copy the flow read, and Cargo's output, both change after the read.
            snapshot_copy = Path(next(call[1] for call in calls if call[0] == "read"))
            replace_file(snapshot_copy, elf_image(TARGET, b"test-root-build"))
            (release / BOOTSTRAP).write_bytes(elf_image(TARGET, b"swapped-after-pin-probe"))
            return good_probes()["--version"]
        self.run_main(release, calls, extra=["--bootstrap-root-sha256", PIN],
                      probes={**good_probes(), "--version": swap})
        with tarfile.open(self.output / "duduclaw-linux-x64.tar.gz") as bundle:
            self.assertEqual(bundle.extractfile(BOOTSTRAP).read(), original)

    def test_no_function_accepts_a_proof_descriptor_or_digest_as_evidence(self):
        # The capability design is gone entirely: nothing to forge, pass or reuse.
        for name in ("BootstrapProof", "_PROOF_ISSUER", "_ISSUED_PROOFS", "verify_bootstrap_binary",
                     "seal_bootstrap_image", "_add_bootstrap_member", "probe_bootstrap", "check_probe", "weakref"):
            self.assertFalse(hasattr(PACKAGE, name), name)
        evidence = {"proof", "bootstrap", "verified", "descriptor", "fd", "fileno", "digest", "sha256",
                    "image", "report", "evidence", "token", "capability"}
        public = {name: function for name, function in inspect.getmembers(PACKAGE, inspect.isfunction)
                  if not name.startswith("_") and function.__module__ == PACKAGE.__name__}
        self.assertIn("create_archive", public)
        for name, function in public.items():
            with self.subTest(function=name):
                self.assertFalse(set(inspect.signature(function).parameters) & evidence)
        self.assertEqual(list(inspect.signature(PACKAGE.create_archive).parameters),
                         ["binary_dir", "python_dir", "target", "output_dir", "binaries", "bootstrap_pin",
                          "bootstrap_version", "expected_bootstrap_sha256"])
        production = PACKAGE.archive_binaries(False)
        path = self.binaries / BOOTSTRAP
        digest = PACKAGE.sha256(path)
        archive, report = self.create(output=self.root / "first")
        # The report is plain data, produced after the flow; nothing takes it back.
        self.assertEqual(report._fields, ("target", "sha256", "size", "pin", "version"))
        self.assertTrue(all(isinstance(value, (str, int)) for value in report))
        for keyword, value in [("verified", {BOOTSTRAP: digest}), ("bootstrap", path), ("proof", report),
                               ("descriptor", 3), ("report", report), ("bootstrap_sha256", digest)]:
            with self.subTest(keyword=keyword), self.assertRaises(TypeError):
                PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output, binaries=production,
                                       **{keyword: value})
        # The verifier is never archived by name, with or without a pin.
        for pin in (None, PIN):
            with self.subTest(pin=pin), self.seam(), self.assertRaisesRegex(ValueError, "never archived as a plain"):
                PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output,
                                       binaries=PACKAGE.archive_binaries(True), bootstrap_pin=pin,
                                       bootstrap_version=VERSION if pin else None)
        # Expectations without a pin are refused, never ignored.
        for keyword, value in [("bootstrap_version", VERSION), ("expected_bootstrap_sha256", digest)]:
            with self.subTest(expectation=keyword), self.assertRaises(ValueError):
                PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output, binaries=production,
                                       **{keyword: value})
        # An expected digest only narrows: the matching digest of a TEST build is still refused
        # by the probe, and a different digest is refused before anything is executed.
        test_probes = {**good_probes(), "--print-root-pin": (0, f"TEST {PIN}\n".encode())}
        with self.assertRaisesRegex(ValueError, "did not print exactly"):
            self.create(probes=test_probes, expected_bootstrap_sha256=digest)
        calls = []
        with self.assertRaisesRegex(ValueError, "differs from the handed-off"):
            self.create(calls=calls, expected_bootstrap_sha256=OTHER_PIN)
        self.assertFalse(any(call[0] == "probe" for call in calls))
        self.assertFalse(self.output.exists())

    def test_a_verifier_for_another_target_is_refused(self):
        x64, arm64 = "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"
        # The host must be Linux of the TARGET's architecture: refused before anything is read.
        for target, host in [(arm64, "x86_64"), (x64, "aarch64"), (x64, None)]:
            calls = []
            with self.subTest(target=target, host=host):
                with self.assertRaisesRegex(ValueError, "cannot be executed on this host"):
                    self.create(target, host=host, calls=calls)
                self.assertEqual(calls, [])
        # A verifier built for the other architecture is refused before it is executed
        # (binfmt emulation could otherwise run, and accept, it); so is a non-ELF.
        for target, content in [(x64, elf_image(arm64)), (arm64, elf_image(x64)), (x64, b"#!/bin/sh\nexit 0\n")]:
            (self.binaries / BOOTSTRAP).write_bytes(content)
            with self.subTest(target=target, content=content[:20]):
                calls = []
                with self.assertRaisesRegex(ValueError, "not a 64-bit little-endian ELF for"):
                    self.create(target, calls=calls)
                self.assertFalse(any(call[0] == "probe" for call in calls))
                with self.seam(host=PACKAGE.TARGETS[target][0], calls=calls), \
                        self.assertRaisesRegex(ValueError, "not a 64-bit little-endian ELF for"):
                    PACKAGE.check_bootstrap_file(self.binaries / BOOTSTRAP, target, PIN, VERSION)
                self.assertFalse(any(call[0] == "probe" for call in calls))
        self.assertFalse(self.output.exists())
        # One run, one target: the archive and the report name the probed target only,
        # and the finished archive is only accepted as that target's archive.
        (self.binaries / BOOTSTRAP).write_bytes(elf_image(arm64))
        archive, report = self.create(arm64)
        self.assertEqual((archive.name, report.target), ("duduclaw-linux-arm64.tar.gz", arm64))
        with self.seam(host="x86_64"), self.assertRaisesRegex(ValueError, "is not the x86_64 release archive"):
            PACKAGE.verify_archive(archive, x64, PIN, VERSION)

    def test_seals_are_verified_before_probing_and_around_the_member_write(self):
        production = PACKAGE.archive_binaries(False)
        archive = self.output / "duduclaw-linux-x64.tar.gz"
        written = []
        reader = PACKAGE._PreadReader

        def create(probes=None, calls=None, substitute_while_writing=False):
            def recording_reader(descriptor, size):
                written.append(size)
                if substitute_while_writing:
                    substitute(descriptor)
                return reader(descriptor, size)
            with self.seam(probes, "x86_64", calls), patch.object(PACKAGE, "_PreadReader", recording_reader):
                return PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output, binaries=production,
                                              bootstrap_pin=PIN, bootstrap_version=VERSION)

        def substitute(descriptor):
            # Same descriptor number, same bytes, but a file without seals.
            copy = unsealed_copy(descriptor)
            os.dup2(copy, descriptor)
            os.close(copy)
        # 1. The image never got its seals: refused before the first probe.
        calls = []
        with self.seam(calls=calls), patch.object(PACKAGE, "_add_seals", lambda descriptor: None), \
                self.assertRaisesRegex(ValueError, "not sealed"):
            PACKAGE.create_archive(self.binaries, self.python, TARGET, self.output, binaries=production,
                                   bootstrap_pin=PIN, bootstrap_version=VERSION)
        self.assertFalse(any(call[0] == "probe" for call in calls))
        # 2. An unsealed descriptor substituted after the probes: refused before the member is written.
        def substituting_probe(descriptor):
            substitute(descriptor)
            return good_probes()["--version"]
        with self.assertRaisesRegex(ValueError, "not sealed"):
            create({**good_probes(), "--version": substituting_probe})
        self.assertEqual(written, [])
        # 3. Substituted while the member is written: refused by the check after the write.
        with self.assertRaisesRegex(ValueError, "not sealed"):
            create(substitute_while_writing=True)
        self.assertEqual(len(written), 1)
        self.assertFalse(archive.exists())
        # Unchanged, the same flow packages.
        archive_path, report = create()
        self.assertEqual(archive_path, archive)
        self.assertEqual(report.sha256, PACKAGE.sha256(self.binaries / BOOTSTRAP))

    def test_image_descriptor_is_closed_on_every_path(self):
        seen = []

        def capturing(result):
            def probe(descriptor):
                seen.append(descriptor)
                if isinstance(result, BaseException):
                    raise result
                return result
            return probe
        good = good_probes()
        cases = [("refused output", {**good, "--print-root-pin": capturing((0, f"TEST {PIN}\n".encode()))},
                  ValueError),
                 ("exec failure", {**good, "--print-root-pin": capturing(OSError(errno.ENOEXEC, "Exec format"))},
                  ValueError),
                 ("interrupted", {**good, "--version": capturing(KeyboardInterrupt())}, KeyboardInterrupt)]
        for name, probes, error in cases:
            with self.subTest(name=name):
                with self.assertRaises(error):
                    self.create(probes=probes)
                with self.assertRaises(OSError):
                    os.fstat(seen[-1])
        with self.seam(probes={**good, "--version": capturing(good["--version"])}):
            report = PACKAGE.check_bootstrap_file(self.binaries / BOOTSTRAP, TARGET, PIN, VERSION)
        self.assertEqual(report.pin, PIN)
        with self.assertRaises(OSError):
            os.fstat(seen[-1])

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

    # ---------------------------------------------------------------- R2: test-key leak prevention

    def test_test_root_pin_in_the_verifier_refuses_the_production_tarball(self):
        # Layer 2: a cfg(duduclaw_test_root) verifier prints TEST <pin>; every
        # packaging path refuses it, with any pin, before an archive exists.
        release = self.cargo_layout()
        test_probes = {**good_probes(), "--print-root-pin": (0, f"TEST {PIN}\n".encode())}
        self.assert_refused(release, ["--bootstrap-root-sha256", PIN], probes=test_probes)
        pin_file = self.root / "bootstrap-root.sha256"
        pin_file.write_text(PIN + "\n")
        self.assert_refused(release, ["--bootstrap-root-pin-file", str(pin_file)], probes=test_probes)
        with self.assertRaises(ValueError):
            self.run_build_only(release, ["--bootstrap-root-sha256", PIN], test_probes)
        # The same refusal on the native host after a hand-off.
        handoff = self.root / "handoff"
        self.run_main(release, [], extra=["--bootstrap-root-sha256", PIN, "--export-binaries", str(handoff)])
        digest = PACKAGE.sha256(handoff / PACKAGE.TRANSFER_SUMS)
        self.assert_refused(release, ["--bootstrap-root-sha256", PIN, "--binary-dir", str(handoff),
                                      "--binary-sums-sha256", digest], probes=test_probes)

    def test_test_signer_is_never_built_listed_or_packaged(self):
        # Layer 1: the TEST-ONLY signer is not a release binary and no release
        # build may name its feature or binary, or select it implicitly.
        self.assertNotIn(PACKAGE.TEST_SIGNER_BINARY, PACKAGE.BIN_PACKAGES)
        PACKAGE.check_release_binary_set()
        for bad in ({**PACKAGE.BIN_PACKAGES, PACKAGE.TEST_SIGNER_BINARY: "duduclaw-updated"},
                    {**PACKAGE.BIN_PACKAGES, "x": "duduclaw-updated/test-signer"}):
            with self.assertRaises(ValueError):
                PACKAGE.check_release_binary_set(bad)
        for include in (True, False):
            command = PACKAGE.build_command(TARGET, "cross", include)
            self.assertEqual(PACKAGE.refuse_test_signer_build(command), command)
            for extra in (["--features", "duduclaw-updated/test-signer"], ["--features=test-signer"],
                          ["--bin", PACKAGE.TEST_SIGNER_BINARY], ["--bins"], ["--all-features"], ["--all-targets"]):
                with self.subTest(extra=extra), self.assertRaises(ValueError):
                    PACKAGE.refuse_test_signer_build(command + extra)
        # Wired into main(): refused before Cargo runs.
        release = self.cargo_layout()
        original = PACKAGE.build_command
        calls = []
        with patch.object(PACKAGE, "build_command",
                          lambda *args: original(*args) + ["--features", "duduclaw-updated/test-signer"]):
            with self.assertRaises(ValueError):
                self.run_main(release, calls, skip_build=False)
        self.assertFalse(any(call[1:2] == ["build"] for call in calls))
        # The Cargo manifest only builds the signer behind its feature.
        manifest = (ROOT / "crates/duduclaw-updated/Cargo.toml").read_text()
        block = re.search(r'\[\[bin\]\]\nname = "duduclaw-isolation-test-signer"\n(?:[^\[\n][^\n]*\n)*', manifest)
        self.assertIsNotNone(block)
        self.assertIn('required-features = ["test-signer"]', block[0])
        self.assertIn('test-signer = ["dep:blake2"]', manifest)
        self.assertNotRegex(manifest, r'(?m)^default\s*=.*test-signer')

    def test_pin_file_absent_means_no_bootstrap_and_present_is_strict(self):
        missing = self.root / "release" / "bootstrap-root.sha256"
        self.assertIsNone(PACKAGE.read_pin_file(missing))
        pin_file = self.root / "pin"
        pin_file.write_text(PIN + "\n")
        self.assertEqual(PACKAGE.read_pin_file(pin_file), PIN)
        for content in [PIN, PIN + "\n\n", PIN.upper() + "\n", PIN + "\r\n", " " + PIN + "\n", "\n",
                        f"{PIN}  root.json\n", "", "ab" * 31 + "\n"]:
            with self.subTest(content=content):
                pin_file.write_text(content)
                with self.assertRaises(ValueError):
                    PACKAGE.read_pin_file(pin_file)
        pin_file.write_text(PIN + "\n")
        link = self.root / "pin-link"
        link.symlink_to(pin_file)
        with self.assertRaises(ValueError):
            PACKAGE.read_pin_file(link)
        self.assertEqual(PACKAGE.resolve_pin(None, pin_file, {}, True), PIN)
        self.assertIsNone(PACKAGE.resolve_pin(None, missing, {}, True))
        for argument, environment in [(PIN, {}), (None, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: OTHER_PIN})]:
            with self.assertRaises(ValueError):
                PACKAGE.resolve_pin(argument, pin_file, environment, True)
        # An inherited pin with an absent file is a conflict, never a silent pin.
        with self.assertRaises(ValueError):
            PACKAGE.resolve_pin(None, missing, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN}, True)
        self.assertEqual(PACKAGE.resolve_pin(None, pin_file, {PACKAGE.BOOTSTRAP_PIN_VARIABLE: PIN}, True), PIN)
        # main(): absent file = SKIP line and no verifier; present file = verified verifier.
        release = self.cargo_layout()
        output = io.StringIO()
        self.run_main(release, [], extra=["--bootstrap-root-pin-file", str(missing)], output=output)
        self.assertIn(PACKAGE.OMITTED_BOOTSTRAP_LINE, output.getvalue().splitlines())
        self.assertNotIn(BOOTSTRAP, self.archive_names())
        shutil.rmtree(self.output)
        calls = []
        self.run_main(release, calls, extra=["--bootstrap-root-pin-file", str(pin_file)], probes=good_probes())
        self.assertIn(BOOTSTRAP, self.archive_names())
        self.assertEqual([call[2] for call in calls if call[0] == "probe"], ["--print-root-pin", "--version"])

    def test_cross_build_forwards_only_the_expected_pin(self):
        environment = PACKAGE.build_environment({"PATH": "/bin", PACKAGE.BOOTSTRAP_PIN_VARIABLE: OTHER_PIN}, None, "cross")
        self.assertNotIn(PACKAGE.BOOTSTRAP_PIN_VARIABLE, environment)
        self.assertNotIn(PACKAGE.CROSS_PASSTHROUGH_VARIABLE, environment)
        environment = PACKAGE.build_environment({PACKAGE.CROSS_PASSTHROUGH_VARIABLE: "OTHER"}, PIN, "cross")
        self.assertEqual(environment[PACKAGE.BOOTSTRAP_PIN_VARIABLE], PIN)
        self.assertEqual(environment[PACKAGE.CROSS_PASSTHROUGH_VARIABLE].split(), ["OTHER", PACKAGE.BOOTSTRAP_PIN_VARIABLE])
        self.assertNotIn(PACKAGE.CROSS_PASSTHROUGH_VARIABLE, PACKAGE.build_environment({}, PIN, "cargo"))
        release = self.cargo_layout()
        self.run_main(release, [], extra=["--bootstrap-root-sha256", PIN], probes=good_probes(), skip_build=False)
        self.assertIn(PACKAGE.BOOTSTRAP_PIN_VARIABLE, self.build_environment[PACKAGE.CROSS_PASSTHROUGH_VARIABLE].split())

    def test_deferred_probe_builds_the_pin_but_never_packages_unprobed(self):
        release = self.cargo_layout()
        # No probe is listed: executing the verifier would fail this test.
        calls = []
        self.run_build_only(release, ["--bootstrap-root-sha256", PIN, "--defer-bootstrap-probe"], {}, calls)
        # Deferred = built, neither read nor executed, and no archive anywhere.
        self.assertEqual(calls, [])
        self.assertFalse(self.output.exists())
        # Packaging never defers, in any mode that could write an archive.
        for extra in (["--skip-build"], ["--skip-build", "--bootstrap-root-sha256", PIN],
                      ["--verify-archive", str(self.root / "duduclaw-linux-x64.tar.gz")]):
            with self.subTest(extra=extra):
                result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET, "--output-dir",
                                         str(self.output), "--defer-bootstrap-probe", *extra],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--defer-bootstrap-probe applies only to --build-only", result.stderr)
                self.assertFalse(self.output.exists())

    def handoff(self, release, include_bootstrap=True):
        handoff = self.root / f"handoff-{os.urandom(6).hex()}"
        extra = ["--bootstrap-root-sha256", PIN] if include_bootstrap else []
        calls = []
        self.run_main(release, calls, extra=[*extra, "--export-binaries", str(handoff)])
        self.assertFalse(any(call[0] == "probe" for call in calls))
        self.assertFalse(self.output.exists())
        # Artifact transit drops the execute bits.
        for path in handoff.iterdir():
            path.chmod(0o644)
        return handoff, PACKAGE.sha256(handoff / PACKAGE.TRANSFER_SUMS)

    def test_native_handoff_rehashes_every_packaged_byte(self):
        release = self.cargo_layout()
        handoff, digest = self.handoff(release)
        self.assertEqual({path.name for path in handoff.iterdir()},
                         {*PACKAGE.archive_binaries(True), PACKAGE.TRANSFER_SUMS})
        self.assertEqual(handoff.stat().st_mode & 0o777, 0o700)
        calls = []
        self.run_main(release, calls, extra=["--bootstrap-root-sha256", PIN, "--binary-dir", str(handoff),
                                             "--binary-sums-sha256", digest], probes=good_probes())
        with tarfile.open(self.output / "duduclaw-linux-x64.tar.gz") as bundle:
            for name in PACKAGE.archive_binaries(True):
                self.assertEqual(bundle.extractfile(name).read(), (release / name).read_bytes())
        # The verifier was read once, from the re-hashed private snapshot (never the
        # hand-off directory), and the executed image is the SHA256SUMS entry.
        reads = [Path(call[1]) for call in calls if call[0] == "read"]
        self.assertEqual(len(reads), 1)
        self.assertNotEqual(reads[0].parent, handoff)
        listed = {name: value for value, name in
                  (line.split("  ") for line in (handoff / PACKAGE.TRANSFER_SUMS).read_text().splitlines())}
        self.assertEqual({call[1] for call in calls if call[0] == "probe"}, {listed[BOOTSTRAP]})
        shutil.rmtree(self.output)
        pinned = ["--bootstrap-root-sha256", PIN]

        def tampered(change):
            copy, _ = self.handoff(release)
            change(copy)
            return copy
        cases = {
            "wrong digest": (handoff, OTHER_PIN),
            "malformed digest": (handoff, digest.upper()),
            "changed binary": (tampered(lambda d: (d / "duduclaw").write_bytes(b"\x7fELF\x00GLIBC_2.17\x00evil")), digest),
            "extra file": (tampered(lambda d: (d / "extra").write_bytes(b"x")), digest),
            "missing binary": (tampered(lambda d: (d / "duduclaw-run").unlink()), digest),
        }
        resigned = tampered(lambda d: (d / "duduclaw").write_bytes(b"\x7fELF\x00GLIBC_2.17\x00evil"))
        lines = (resigned / PACKAGE.TRANSFER_SUMS).read_text().splitlines()
        lines = [f"{PACKAGE.sha256(resigned / 'duduclaw')}  duduclaw" if line.endswith("  duduclaw") else line
                 for line in lines]
        (resigned / PACKAGE.TRANSFER_SUMS).write_text("\n".join(lines) + "\n")
        cases["rewritten SHA256SUMS"] = (resigned, digest)
        unpinned, unpinned_digest = self.handoff(release, include_bootstrap=False)
        cases["set built without the pin"] = (unpinned, unpinned_digest)
        for name, (directory, value) in cases.items():
            with self.subTest(name=name):
                self.assert_refused(release, [*pinned, "--binary-dir", str(directory), "--binary-sums-sha256", value],
                                    probes=good_probes())
        for extra in (["--binary-dir", str(handoff)], ["--binary-sums-sha256", digest]):
            result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET, "--skip-build",
                                     "--output-dir", str(self.output), *extra], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("go together", result.stderr)

    def archive_with(self, include_bootstrap, name):
        return self.package(include_bootstrap=include_bootstrap, output=self.root / name)

    def test_verify_archive_reports_presence_and_absence(self):
        without = self.archive_with(False, "without")
        line = PACKAGE.verify_archive(without, TARGET, None, VERSION)
        self.assertTrue(line.startswith(f"x86_64: {BOOTSTRAP} NOT shipped"), line)
        with self.seam(), self.assertRaises(ValueError):
            PACKAGE.verify_archive(without, TARGET, PIN, VERSION)
        shipped = self.archive_with(True, "with")
        with self.assertRaisesRegex(ValueError, "no production root pin"):
            PACKAGE.verify_archive(shipped, TARGET, None, VERSION)
        executed = PACKAGE.sha256(self.binaries / BOOTSTRAP)
        calls = []
        # Executed from a sealed copy of exactly the member bytes: nothing is extracted
        # to a path and no path is read for the verifier.
        with self.seam(calls=calls), patch.object(PACKAGE, "extract_member", side_effect=AssertionError):
            line = PACKAGE.verify_archive(shipped, TARGET, PIN, VERSION)
        self.assertIn(f"PRODUCTION {PIN}, sha256 {executed}", line)
        self.assertFalse(any(call[0] == "read" for call in calls))
        self.assertEqual([(call[1], call[2]) for call in calls if call[0] == "probe"],
                         [(executed, "--print-root-pin"), (executed, "--version")])
        test_probes = {**good_probes(), "--print-root-pin": (0, f"TEST {PIN}\n".encode())}
        with self.seam(test_probes), self.assertRaisesRegex(ValueError, "did not print exactly"):
            PACKAGE.verify_archive(shipped, TARGET, PIN, VERSION)
        with self.seam(host="aarch64"), self.assertRaisesRegex(ValueError, "cannot be executed on this host"):
            PACKAGE.verify_archive(shipped, TARGET, PIN, VERSION)
        Path(str(shipped) + ".sha256").write_text("00" * 32 + "  duduclaw-linux-x64.tar.gz\n")
        with self.assertRaisesRegex(ValueError, "does not match its .sha256"):
            PACKAGE.verify_archive(shipped, TARGET, None, VERSION)
        with self.assertRaisesRegex(ValueError, "is not the aarch64 release archive"):
            PACKAGE.verify_archive(without, "aarch64-unknown-linux-gnu", None, VERSION)

    def test_released_archive_is_read_only_through_its_checksum(self):
        shipped = self.archive_with(True, "released")
        snapshot = self.root / "snapshot"
        snapshot.mkdir(mode=0o700)
        names, digest = PACKAGE.snapshot_release_archive(shipped, TARGET, snapshot)
        self.assertEqual(names, PACKAGE.archive_binaries(False))
        self.assertEqual(digest, PACKAGE.sha256(shipped))
        self.assertEqual({path.name for path in snapshot.iterdir()}, set(names))
        for name in names:
            self.assertEqual((snapshot / name).read_bytes(), (self.binaries / name).read_bytes())
        asset, asset_digest = PACKAGE.extract_bootstrap(shipped, TARGET, self.output)
        self.assertEqual(asset.name, "duduclaw-bootstrap-linux-x86_64")
        self.assertEqual(asset.read_bytes(), (self.binaries / BOOTSTRAP).read_bytes())
        self.assertEqual(asset_digest, PACKAGE.sha256(asset))
        with self.assertRaisesRegex(ValueError, "already exists"):
            PACKAGE.extract_bootstrap(shipped, TARGET, self.output)
        with self.assertRaises(ValueError):
            PACKAGE.extract_bootstrap(self.archive_with(False, "unpinned"), TARGET, self.root / "other")
        for arguments, message in [
                (["--from-release-archive", str(shipped), "--bootstrap-root-pin-file", str(self.root / "x")],
                 "never packages the verifier"),
                (["--test-signer", str(self.root / "signer"), "--skip-build"], "TEST-ONLY"),
                (["--from-release-archive", str(shipped), "--test-signer", str(self.root / "signer"),
                  "--bootstrap-root-sha256", PIN], "TEST-ONLY"),
                (["--from-release-archive", str(shipped), "--skip-build"], "does not build or package"),
                (["--verify-archive", str(shipped), "--extract-bootstrap", str(shipped)], "mutually exclusive"),
                (["--manifest-valid-days", "30", "--skip-build"], "belongs to --from-release-archive")]:
            with self.subTest(arguments=arguments):
                result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET, "--output-dir",
                                         str(self.root / "cli"), *arguments], capture_output=True, text=True,
                                        env={**os.environ, "CI": "", "GITHUB_ACTIONS": ""})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def test_release_template_renders_into_a_signable_manifest(self):
        raw = (ROOT / "release/isolation/manifest-template.json").read_bytes()
        base = json.loads(raw)
        self.assertFalse(set(PACKAGE.PER_RELEASE_FIELDS) & set(base))
        version = {"major": 1, "minor": 83, "patch": 0}
        rendered = json.loads(PACKAGE.render_release_template(raw, "aarch64", version, 1_800_000_000, 365))
        self.assertEqual(rendered["version"], version)
        self.assertEqual(rendered["arch"], "aarch64")
        self.assertEqual(rendered["serial"], 1_083_000)
        self.assertEqual(rendered["created_at_unix"], 1_800_000_000)
        self.assertEqual(rendered["expires_at_unix"], 1_800_000_000 + 365 * 86400)
        self.assertEqual((rendered["schema_version"], rendered["product"], rendered["channel"], rendered["os"]),
                         (1, "duduclaw", "stable", "linux"))
        PACKAGE.validate_targets(rendered["artifacts"])
        # Artifact ids are exactly the names the update service downloads (publication.rs).
        source = (ROOT / "crates/duduclaw-updated/src/publication.rs").read_text()
        for artifact in rendered["artifacts"]:
            variant = "".join(part.title() for part in artifact["install_target"]["kind"].split("_"))
            match = re.search(r'InstallTarget::' + variant + r' => "([^"]+)"', source)
            self.assertEqual(artifact["id"], match[1])
        # The workspace version renders too (the template never outruns the tree).
        PACKAGE.render_release_template(raw, "x86_64", PACKAGE.workspace_version(), 1_800_000_000, 1)
        serials = [PACKAGE.release_serial({"major": a, "minor": b, "patch": c})
                   for a, b, c in [(1, 82, 9), (1, 83, 0), (1, 83, 1), (1, 84, 0), (2, 0, 0)]]
        self.assertEqual(serials, sorted(set(serials)))
        for bad_version in [{"major": 1, "minor": 1000, "patch": 0}, {"major": 1, "minor": 83, "patch": 1000}]:
            with self.assertRaises(ValueError):
                PACKAGE.release_serial(bad_version)
        for field in PACKAGE.PER_RELEASE_FIELDS:
            with self.subTest(field=field), self.assertRaises(ValueError):
                PACKAGE.render_release_template(json.dumps({**base, field: 1}).encode(), "x86_64", version, 1, 1)
        for days in (0, PACKAGE.MAX_VALID_DAYS + 1, True):
            with self.assertRaises(ValueError):
                PACKAGE.render_release_template(raw, "x86_64", version, 1, days)
        with self.assertRaises(ValueError):
            PACKAGE.render_release_template(raw, "x86_64", {"major": 1, "minor": 81, "patch": 0}, 1, 1)

    def ustar_members(self, path):
        """Parse the bundle with the verifier's rules (duduclaw-updated bundle.rs)."""
        image = path.read_bytes()
        members, offset = [], 0
        while image[offset:offset + 512] != bytes(512):
            header = image[offset:offset + 512]
            self.assertEqual(header[257:265], b"ustar\x0000")
            self.assertEqual(header[156:157], b"0")
            self.assertFalse(any(header[157:257]))
            checksum = int(header[148:156].rstrip(b"\0 ").decode(), 8)
            self.assertEqual(checksum, sum(header[:148]) + 256 + sum(header[156:]))
            name = header[:100].split(b"\0", 1)[0].decode()
            size = int(header[124:136].rstrip(b"\0 ").decode(), 8)
            self.assertEqual(int(header[100:108].rstrip(b"\0 ").decode(), 8), 0o600)
            data = image[offset + 512:offset + 512 + size]
            members.append((name, data))
            padded = (size + 511) // 512 * 512
            self.assertFalse(any(image[offset + 512 + size:offset + 512 + padded]))
            offset += 512 + padded
        self.assertGreaterEqual(len(image) - offset, 1024)
        self.assertFalse(any(image[offset:]))
        return members

    def test_migration_bundle_is_plain_ustar_in_inbox_order(self):
        root, signatures = self.bootstrap_inputs()
        inbox = PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        bundle = PACKAGE.create_migration_bundle(TARGET, inbox, self.output)
        self.assertEqual(bundle.name, f"duduclaw-isolated-linux-x86_64-{VERSION}.tar")
        members = self.ustar_members(bundle)
        manifest = json.loads((inbox / "manifest.json").read_bytes())
        expected = [*PACKAGE.BUNDLE_METADATA, *("artifacts/" + artifact["id"] for artifact in manifest["artifacts"])]
        self.assertEqual([name for name, _ in members], expected)
        for name, data in members:
            self.assertEqual(data, (inbox / name).read_bytes())
        with self.assertRaisesRegex(ValueError, "already exists"):
            PACKAGE.create_migration_bundle(TARGET, inbox, self.output)
        other = self.root / "other-output"
        (inbox / "artifacts" / "gateway").chmod(0o600)
        original = (inbox / "artifacts" / "gateway").read_bytes()
        for name, change, revert in [
                ("changed artifact", lambda: (inbox / "artifacts" / "gateway").write_bytes(b"changed"),
                 lambda: (inbox / "artifacts" / "gateway").write_bytes(original)),
                ("extra artifact", lambda: (inbox / "artifacts" / "unsigned").write_bytes(b"x"),
                 lambda: (inbox / "artifacts" / "unsigned").unlink()),
                ("extra metadata", lambda: (inbox / "notes.txt").write_bytes(b"x"),
                 lambda: (inbox / "notes.txt").unlink())]:
            with self.subTest(name=name):
                change()
                with self.assertRaises(ValueError):
                    PACKAGE.create_migration_bundle(TARGET, inbox, other)
                self.assertFalse((other / bundle.name).exists())
                revert()
        with self.assertRaisesRegex(ValueError, "version and target architecture"):
            PACKAGE.create_migration_bundle("aarch64-unknown-linux-gnu", inbox, other)

    def test_verifier_is_never_a_signed_artifact_inbox_file_or_bundle_member(self):
        # The only way into any archive is create_archive's sealed flow; the signed
        # manifest (and so the inbox and the migration bundle) can never carry it.
        for identifier in (BOOTSTRAP, PACKAGE.BOOTSTRAP_ASSET_PREFIX + "x86_64"):
            with self.subTest(identifier=identifier):
                document = self.manifest()
                document["artifacts"].append({"id": identifier, "mode": 0o755,
                                              "install_target": {"kind": "runtime_entry", "registry_id": "vendor-x"}})
                with self.assertRaisesRegex(ValueError, "never a signed artifact"):
                    PACKAGE.validate_targets(document["artifacts"])
        root, signatures = self.bootstrap_inputs()
        inbox = PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        manifest = json.loads((inbox / "manifest.json").read_bytes())
        manifest["artifacts"][0]["id"] = BOOTSTRAP
        (inbox / "manifest.json").write_bytes(json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode())
        (inbox / "artifacts" / manifest["artifacts"][0]["install_target"]["kind"]).rename(inbox / "artifacts" / BOOTSTRAP)
        with self.assertRaisesRegex(ValueError, "never a signed artifact"):
            PACKAGE.create_migration_bundle(TARGET, inbox, self.root / "bundle")
        self.assertFalse((self.root / "bundle" / PACKAGE.migration_bundle_name("x86_64", PACKAGE.workspace_version())).exists())

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
