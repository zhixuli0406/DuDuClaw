"""scripts/refuse-test-license-build.py: the one TEST-licence refuser every release path runs."""
import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/refuse-test-license-build.py"
SPEC = importlib.util.spec_from_file_location("test_license_refuser_under_test", SCRIPT)
REFUSER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFUSER)
LICENSE_RUNTIME = ROOT / "crates/duduclaw-gateway/src/license_runtime.rs"
# Read from the Rust source here as well: no test or script keeps its own copy.
MARKER = re.search(rb'static TEST_LICENSE_BUILD_MARKER: &\[u8\] = b"([^"]+)";', LICENSE_RUNTIME.read_bytes())[1]
TARGET = "x86_64-unknown-linux-gnu"

try:
    PACKAGER_SPEC = importlib.util.spec_from_file_location("linux_release_for_refuser", ROOT / "scripts/package-linux-release.py")
    PACKAGE = importlib.util.module_from_spec(PACKAGER_SPEC)
    PACKAGER_SPEC.loader.exec_module(PACKAGE)
except ImportError:  # fcntl: the Linux packager does not import on Windows
    PACKAGE = None


def gateway(marked, size=4096):
    body = b"\x7fELF" + b"\x00" * size
    return body + MARKER + b"\x00tail" if marked else body + MARKER[:-1] + b"\x00tail"


class RefuserTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        # Resolved: the refuser names configuration files by their resolved path (macOS /private/var).
        self.root = Path(self.temporary.name).resolve()

    def run_main(self, *argv):
        """(exit status, stdout, stderr) of main(argv)."""
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                status = REFUSER.main(list(map(str, argv)))
            except SystemExit as exit:  # argparse usage errors
                status = exit.code
        return status, out.getvalue(), err.getvalue()

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def tar(self, name, members):
        path = self.root / name
        with tarfile.open(path, "w:gz") as bundle:
            for member, data in members:
                info = tarfile.TarInfo(member)
                info.size = len(data)
                bundle.addfile(info, io.BytesIO(data))
        return path

    def zip(self, name, members):
        path = self.root / name
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as bundle:
            for member, data in members:
                bundle.writestr(member, data)
        return path

    # ---------------------------------------------------------------- the marker

    def test_marker_is_parsed_from_the_one_rust_definition(self):
        self.assertEqual(REFUSER.load_marker(), MARKER)
        self.assertGreaterEqual(len(MARKER), REFUSER.MIN_MARKER_BYTES)
        source = LICENSE_RUNTIME.read_bytes()
        self.assertEqual(source.count(MARKER), 1)
        # Compiled only into a test-root build.
        self.assertIn(b'#[cfg(duduclaw_test_root)]\n#[used]\nstatic TEST_LICENSE_BUILD_MARKER: &[u8] = b"' + MARKER
                      + b'";', source)
        definition = b'static TEST_LICENSE_BUILD_MARKER: &[u8] = b"' + MARKER + b'";\n'
        for bad in (b"", b"// nothing here\n", definition + definition,
                    definition + b"const TEST_LICENSE_BUILD_MARKER: &[u8] = b\"OTHER-MARKER-VALUE-123\";\n",
                    b'static TEST_LICENSE_BUILD_MARKER: &[u8] = b"SHORT";\n',
                    b'    static TEST_LICENSE_BUILD_MARKER: &[u8] = b"' + MARKER + b'";\n'):
            with self.subTest(bad=bad), self.assertRaises(REFUSER.RefuserError):
                REFUSER.parse_marker(bad)
        self.assertEqual(REFUSER.parse_marker(b"x\n" + definition + b"y\n"), MARKER)

    def test_old_source_passes_without_scanning(self):
        source = self.write("old/crates/duduclaw-gateway/src/license_runtime.rs", b"// Old licence runtime\n")
        with patch.object(REFUSER, "LICENSE_RUNTIME", source), patch.object(REFUSER, "scan") as scan:
            status, out, err = self.run_main(self.root / "absent-binary")
        self.assertEqual(status, 0)
        self.assertEqual(out.splitlines(), [
            "refuse-test-license-build: source predates the test-licence issuer; skipping marker scan"])
        self.assertEqual(err, "")
        scan.assert_not_called()

    def test_source_marker_errors_fail_closed(self):
        definition = b'static TEST_LICENSE_BUILD_MARKER: &[u8] = b"' + MARKER + b'";\n'
        for content in (b"#[cfg(duduclaw_test_root)]\n", definition + definition):
            with self.subTest(content=content):
                source = self.write("invalid/crates/duduclaw-gateway/src/license_runtime.rs", content)
                with patch.object(REFUSER, "LICENSE_RUNTIME", source), patch.object(REFUSER, "scan") as scan:
                    status, out, err = self.run_main(self.root / "absent-binary")
                self.assertEqual(status, 2)
                self.assertEqual(out, "")
                self.assertIn("exactly once", err)
                scan.assert_not_called()
        with patch.object(REFUSER, "LICENSE_RUNTIME", self.root / "missing-source"):
            self.assertEqual(self.run_main(self.root / "absent-binary")[0], 2)

    def test_no_release_tooling_keeps_a_copy_of_the_marker(self):
        files = [*(ROOT / "scripts").glob("*.py"), *(ROOT / "scripts").glob("*.sh"), ROOT / "scripts/release.sh",
                 *(ROOT / ".github/workflows").glob("*.yml"), ROOT / "container/Dockerfile.server",
                 ROOT / "npm/scripts/publish.sh", ROOT / "release/isolation/README.md",
                 *(ROOT / "scripts/tests").glob("test_*.py")]
        for path in files:
            with self.subTest(path=path.relative_to(ROOT)):
                self.assertNotIn(MARKER, path.read_bytes())

    def test_marker_file_replaces_the_source_and_is_strict(self):
        marked = self.write("bin/duduclaw", gateway(True))
        marker_file = self.write("marker", MARKER + b"\n")
        self.assertEqual(self.run_main("--marker-file", marker_file, marked)[0], 1)
        other = self.write("other-marker", b"SOME-OTHER-MARKER-VALUE")
        self.assertEqual(self.run_main("--marker-file", other, marked)[0], 0)
        for content in (b"", b"\n", b"SHORT\n", MARKER + b"\n\n", MARKER + b" \n", b"two\nlines-of-marker-text"):
            with self.subTest(content=content):
                bad = self.write("bad-marker", content)
                status, _, err = self.run_main("--marker-file", bad, marked)
                self.assertEqual(status, 2)
                self.assertIn("--marker-file", err)
        self.assertEqual(self.run_main("--marker-file", self.root / "absent", marked)[0], 2)

    # ---------------------------------------------------------------- binaries and archives

    def test_raw_binaries(self):
        clean = self.write("clean/duduclaw", gateway(False))
        marked = self.write("anything.bin", gateway(True))
        status, out, _ = self.run_main(clean)
        self.assertEqual(status, 0)
        self.assertIn("OK", out)
        status, _, err = self.run_main(clean, marked)
        self.assertEqual(status, 1)
        self.assertEqual(len(err.splitlines()), 1)
        self.assertIn(f"REFUSED {marked} is a TEST-licence gateway build", err)
        # Found across the read-chunk border, and a near miss is not a match.
        size = REFUSER.CHUNK_BYTES - 5
        border = self.write("border", b"x" * size + MARKER + b"y")
        self.assertEqual(self.run_main(border)[0], 1)
        near = self.write("near", b"x" * size + MARKER[:-1] + b"z" + MARKER[1:])
        self.assertEqual(self.run_main(near)[0], 0)

    def test_tar_and_zip_gateway_members(self):
        for name, build in (("release.tar.gz", self.tar), ("release.tgz", self.tar), ("release.zip", self.zip)):
            for member in ("duduclaw", "duduclaw.exe", "duduclaw-pro", "duduclaw-pro.exe", "duduclaw-spawnd",
                           "DuDuClaw Gateway.app/Contents/MacOS/duduclaw", "bin\\duduclaw.exe"):
                with self.subTest(archive=name, member=member):
                    clean = build("clean-" + name, [("python/x.py", b"print()"), (member, gateway(False))])
                    status, out, _ = self.run_main(clean)
                    self.assertEqual(status, 0)
                    self.assertIn("1 gateway binaries scanned", out)
                    marked = build("marked-" + name, [("python/x.py", b"print()"), (member, gateway(True))])
                    status, _, err = self.run_main(marked)
                    self.assertEqual(status, 1)
                    self.assertIn(f"REFUSED {marked}:{member} is a TEST-licence gateway build", err)

    def test_non_gateway_members_are_ignored(self):
        members = [("README.md", MARKER), ("python/duduclaw/__init__.py", MARKER),
                   ("duduclaw-linux-x64.tar.gz", MARKER), ("docs/duduclaw-notes.txt", MARKER),
                   ("duduclaw", gateway(False))]
        for archive in (self.tar("mixed.tar.gz", members), self.zip("mixed.zip", members)):
            with self.subTest(archive=archive.name):
                status, out, _ = self.run_main(archive)
                self.assertEqual(status, 0)
                self.assertIn("1 gateway binaries scanned", out)

    def test_absolute_and_parent_member_names_are_read_never_extracted(self):
        before = sorted(os.listdir(self.root))
        for member in ("/abs/duduclaw", "../duduclaw", "../../escape/duduclaw-pro"):
            with self.subTest(member=member):
                archive = self.tar("names.tar.gz", [(member, gateway(True))])
                cwd = os.getcwd()
                os.chdir(self.root)
                try:
                    status = self.run_main(archive)[0]
                finally:
                    os.chdir(cwd)
                self.assertEqual(status, 1)
                self.assertEqual(sorted(os.listdir(self.root)), sorted(set(before) | {"names.tar.gz"}))
                self.assertFalse((self.root.parent / "duduclaw").exists())

    def test_unreadable_inputs_exit_2(self):
        junk_tar = self.write("junk.tar.gz", b"not a gzip stream at all")
        junk_zip = self.write("junk.zip", b"PK\x03\x04 broken")
        # Incompressible member bytes, so cutting the tail really truncates the member.
        full = self.tar("full.tar.gz", [("duduclaw", os.urandom(300000))])
        truncated = self.write("truncated.tar.gz", full.read_bytes()[:-2000])
        link = self.root / "link.tar.gz"
        with tarfile.open(link, "w:gz") as bundle:
            info = tarfile.TarInfo("duduclaw")
            info.type = tarfile.SYMTYPE
            info.linkname = "python/elsewhere"
            bundle.addfile(info)
        directory = self.root / "a-directory"
        directory.mkdir()
        for path in (junk_tar, junk_zip, truncated, link, directory, self.root / "missing"):
            with self.subTest(path=path.name):
                status, _, err = self.run_main(path)
                self.assertEqual(status, 2)
                self.assertIn("ERROR", err)
        # A refusal wins over an error elsewhere; both are non-zero.
        self.assertEqual(self.run_main(junk_tar, self.write("m", gateway(True)))[0], 1)
        # Usage errors.
        self.assertEqual(self.run_main()[0], 2)
        self.assertEqual(self.run_main("--target", TARGET, full)[0], 2)

    def test_cli_runs_isolated_and_reports_exit_codes(self):
        marked = self.write("cli/duduclaw", gateway(True))
        clean = self.write("cli/clean", gateway(False))
        for path, expected in ((clean, 0), (marked, 1), (self.root / "nope.zip", 2)):
            with self.subTest(path=path.name):
                result = subprocess.run([sys.executable, "-I", "-B", str(SCRIPT), str(path)],
                                        capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, expected, result.stderr)

    # ---------------------------------------------------------------- build environment

    def problems(self, environment, target=None, cargo_dirs=None):
        home = self.root / "cargo-home"
        home.mkdir(exist_ok=True)
        start = self.root / "repo"
        start.mkdir(exist_ok=True)
        return REFUSER.environment_problems(environment, target, cargo_dirs or [start], cargo_home=home)

    def test_environment_variables(self):
        refused = [{"RUSTFLAGS": "-C opt-level=s --cfg duduclaw_test_root"},
                   {"RUSTFLAGS": "--cfg=duduclaw_test_root"},
                   {"RUSTFLAGS": "--cfg  duduclaw_test_root=\"1\""},
                   {"CARGO_ENCODED_RUSTFLAGS": "-Copt-level=s\x1f--cfg\x1fduduclaw_test_root"},
                   {"CARGO_BUILD_RUSTFLAGS": "--cfg 'duduclaw_test_root'"},
                   {"CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS": "--cfg duduclaw_test_root"},
                   {"CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS": "--cfg duduclaw_test_root"},
                   {REFUSER.TEST_LICENSE_ISSUER_VARIABLE: "ab" * 32},
                   {REFUSER.TEST_LICENSE_ISSUER_VARIABLE: ""},
                   {REFUSER.BOOTSTRAP_TEST_KEY_VARIABLE: "RWQtest"},
                   *({variable: "sccache"} for variable in REFUSER.RUSTC_VARIABLES),
                   *({variable: ""} for variable in REFUSER.RUSTC_VARIABLES)]
        for environment in refused:
            with self.subTest(environment=environment):
                problems = self.problems(environment)
                self.assertEqual(len(problems), 1, problems)
                self.assertIn(next(iter(environment)), problems[0])
        for environment in ({}, {"RUSTFLAGS": "--cfg duduclaw_test_root_extra"},
                            {"RUSTFLAGS": "-Cmetadata=duduclaw_test_root"}, {"CARGO_ENCODED_RUSTFLAGS": "--cfg\x1fother"},
                            {"DUDUCLAW_LICENSE_PUBKEY_V2": "ab" * 32}, {"CARGO_HOME": "/elsewhere", "PATH": "/bin"}):
            with self.subTest(clean=environment):
                self.assertEqual(self.problems(environment), [])
        # Without --target every target's flags count; with it only that target's (packager rule).
        other = {"CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS": "--cfg duduclaw_test_root"}
        self.assertEqual(len(self.problems(other)), 1)
        self.assertEqual(self.problems(other, target=TARGET), [])
        self.assertEqual(len(self.problems(other, target="aarch64-unknown-linux-gnu")), 1)
        # Every problem is reported.
        self.assertEqual(len(self.problems({"RUSTC_WRAPPER": "x", "RUSTFLAGS": "--cfg duduclaw_test_root"})), 2)

    def test_cargo_configuration_in_the_tree_its_parents_and_cargo_home(self):
        tree = self.root / "parent/repo/crate"
        tree.mkdir(parents=True)
        self.assertEqual(self.problems({}, cargo_dirs=[tree]), [])
        for directory, name in ((self.root / "parent/repo", "config.toml"), (self.root / "parent", "config"),
                                (tree, "config.toml"), (self.root / "cargo-home", "config.toml")):
            with self.subTest(directory=directory, name=name):
                config = directory / (name if directory.name == "cargo-home" else f".cargo/{name}")
                config.parent.mkdir(parents=True, exist_ok=True)
                config.write_text('[build]\nrustflags = ["--cfg", "duduclaw_test_root"]\n')
                problems = self.problems({}, cargo_dirs=[tree])
                self.assertEqual(len(problems), 1, problems)
                self.assertIn(str(config), problems[0])
                config.write_text('[env]\nPYO3_PYTHON = "/usr/bin/python3"\n')
                self.assertEqual(self.problems({}, cargo_dirs=[tree]), [])
        # $CARGO_HOME from the environment when none is injected.
        home = self.root / "env-home"
        home.mkdir()
        (home / "config.toml").write_text('[target.x86_64-unknown-linux-gnu]\nrustflags = "--cfg=duduclaw_test_root"\n')
        self.assertEqual(len(REFUSER.environment_problems({"CARGO_HOME": str(home)}, None, [tree])), 1)

    def test_cargo_rustc_overrides_in_table_and_dotted_forms(self):
        config = self.write("repo/.cargo/config.toml", b"")
        parsers = (REFUSER.tomllib, None) if REFUSER.tomllib is not None else (None,)
        for parser in parsers:
            for key in REFUSER.RUSTC_CONFIG_KEYS:
                for content in (f'[build]\n{key} = "compiler-shim"\n',
                                f'build.{key} = "compiler-shim"\n'):
                    with self.subTest(parser=parser, content=content), patch.object(REFUSER, "tomllib", parser):
                        config.write_text(content)
                        problems = self.problems({})
                        self.assertEqual(len(problems), 1, problems)
                        self.assertIn(str(config), problems[0])
                        self.assertIn(f"build.{key}", problems[0])
                        config.write_text("\n".join("# " + line for line in content.splitlines()))
                        self.assertEqual(self.problems({}), [])
                        config.write_text(f'[build]\n# {key} = "compiler-shim"\n')
                        self.assertEqual(self.problems({}), [])

    def test_cargo_config_unrelated_settings_and_parse_errors(self):
        config = self.write("repo/.cargo/config", b"")
        parsers = (REFUSER.tomllib, None) if REFUSER.tomllib is not None else (None,)
        for parser in parsers:
            with self.subTest(parser=parser), patch.object(REFUSER, "tomllib", parser):
                config.write_text('[net]\noffline = true\n[target.x86_64-unknown-linux-gnu]\n'
                                  'linker = "cc"\nrunner = "emulator"\n')
                self.assertEqual(self.problems({}), [])
                for content in (b"[build\n", b'[build]\nrustc-wrapper = "unterminated\n',
                                b"\xff", b"[net]\noffline = true\noffline = false\n"):
                    with self.subTest(content=content):
                        config.write_bytes(content)
                        with self.assertRaisesRegex(REFUSER.RefuserError, re.escape(str(config))):
                            self.problems({})
                config.write_text("# duduclaw_test_root\n")
                self.assertEqual(len(self.problems({})), 1)

    def test_cargo_config_unreadable_file_fails_closed(self):
        config = self.write("repo/.cargo/config.toml", b"[net]\noffline = true\n")
        read_bytes = Path.read_bytes

        def unreadable(path):
            if path == config:
                raise PermissionError("permission denied")
            return read_bytes(path)

        with patch.object(Path, "read_bytes", unreadable):
            with self.assertRaisesRegex(REFUSER.RefuserError, re.escape(str(config))):
                self.problems({})
        config.unlink()
        config.mkdir()
        with self.assertRaisesRegex(REFUSER.RefuserError, re.escape(str(config))):
            self.problems({})

    def test_cargo_config_includes_are_refused(self):
        config = self.write("repo/.cargo/config.toml", b"")
        parsers = (REFUSER.tomllib, None) if REFUSER.tomllib is not None else (None,)
        for parser in parsers:
            for value in ('"extra.toml"', '["one.toml", "two.toml"]', 'false', '42'):
                with self.subTest(parser=parser, value=value), patch.object(REFUSER, "tomllib", parser):
                    config.write_text(f"include = {value}\n")
                    problems = self.problems({})
                    self.assertTrue(problems)
                    self.assertIn(str(config), problems[0])
                    self.assertIn("include is not allowed in a release environment", problems[0])

    @unittest.skipIf(REFUSER.tomllib is None, "decoded TOML requires Python 3.11+")
    def test_cargo_config_decoded_cfg_in_keys_and_values_is_refused(self):
        config = self.write("repo/.cargo/config.toml", b"")
        escaped = r"duduclaw_\u0074est_root"
        for content in (f'[build]\nrustflags = ["--cfg", "{escaped}"]\n',
                        f'[build]\nrustflags = [\n"--cfg",\n"{escaped}",\n]\n',
                        f'[alias]\n"{escaped}" = "check"\n',
                        f'[alias]\n"{escaped}" = {{ nested = ["check"] }}\n',
                        f'[alias]\nnested = {{ values = ["{escaped}"] }}\n',
                        '[build]\nrustflags = ["--cfg", """duduclaw_\\\n  test_root"""]\n'):
            with self.subTest(content=content):
                config.write_text(content)
                self.assertNotIn(REFUSER.TEST_ROOT_CFG.encode(), config.read_bytes())
                problems = self.problems({})
                self.assertTrue(problems)
                self.assertIn(str(config), problems[0])
                self.assertIn(REFUSER.TEST_ROOT_CFG, problems[0])

    def test_cargo_config_fallback_refuses_unsafe_decoding(self):
        config = self.write("repo/.cargo/config.toml", b"")
        with patch.object(REFUSER, "tomllib", None):
            for content in ('# \\u0074\n', '# \\U00000074\n', '# """\n', "# '''\n",
                            '[alias]\ncheck = """check"""\n'):
                with self.subTest(content=content):
                    config.write_text(content)
                    with self.assertRaisesRegex(REFUSER.RefuserError, re.escape(str(config))):
                        self.problems({})

    def test_cargo_config_env_names_are_refused(self):
        config = self.write("repo/.cargo/config.toml", b"")
        names = (*REFUSER.RELEASE_ENV_VARIABLES, 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS',
                 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTC',
                 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTC_WRAPPER',
                 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTC_WORKSPACE_WRAPPER')
        parsers = (REFUSER.tomllib, None) if REFUSER.tomllib is not None else (None,)
        for parser in parsers:
            for name in names:
                for content in (f'[env]\n{name} = "value"\n', f'env.{name} = "value"\n'):
                    with self.subTest(parser=parser, content=content), patch.object(REFUSER, "tomllib", parser):
                        config.write_text(content)
                        problems = self.problems({})
                        self.assertTrue(problems)
                        self.assertIn(str(config), problems[0])
                        self.assertIn(name, problems[0])

    def test_cargo_config_legitimate_release_settings_are_accepted(self):
        config = self.write("repo/.cargo/config.toml", b"")
        parsers = (REFUSER.tomllib, None) if REFUSER.tomllib is not None else (None,)
        for parser in parsers:
            for content in ((ROOT / ".cargo/config.toml").read_text(),
                            '[env]\nPYO3_PYTHON = "/usr/bin/python3"\n',
                            '[target.x86_64-unknown-linux-gnu]\nlinker = "x"\n'
                            'rustflags = ["-C", "target-cpu=x86-64-v2"]\n',
                            '[net]\noffline = true\n[profile.release]\nopt-level = 3\n'
                            '[alias]\nb = "build"\n[registries.internal]\nindex = "https://example.com"\n'
                            '[source.crates-io]\nreplace-with = "internal"\n'):
                with self.subTest(parser=parser, content=content), patch.object(REFUSER, "tomllib", parser):
                    config.write_text(content)
                    self.assertEqual(self.problems({}), [])

    def test_cargo_rustc_overrides_in_parents_and_cargo_home(self):
        tree = self.root / "parent/repo/crate"
        tree.mkdir(parents=True)
        for name in ("parent/.cargo/config", "parent/repo/.cargo/config.toml",
                     "cargo-home/config.toml"):
            with self.subTest(name=name):
                config = self.write(name, b'[build]\nrustc-wrapper = "compiler-shim"\n')
                problems = self.problems({}, cargo_dirs=[tree])
                self.assertEqual(len(problems), 1, problems)
                self.assertIn(str(config), problems[0])
                config.unlink()
        config = self.write("env-home/config", b'build.rustc = "compiler-shim"\n')
        problems = REFUSER.environment_problems({"CARGO_HOME": str(config.parent)}, cargo_dirs=[tree])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn(str(config), problems[0])

    def test_cargo_config_quoted_keys_and_conservative_fallback(self):
        config = self.write("repo/.cargo/config.toml", b"")
        for content in ('["build"]\n"rustc-wrapper" = "shim"\n',
                        '"build"."rustc" = "shim"\n',
                        'build = { rustc-workspace-wrapper = "shim" }\n',
                        '[build.rustc-wrapper]\n'):
            with self.subTest(content=content):
                config.write_text(content)
                if REFUSER.tomllib is not None:
                    self.assertTrue(self.problems({}))
                with patch.object(REFUSER, "tomllib", None):
                    try:
                        problems = self.problems({})
                    except REFUSER.RefuserError as error:
                        self.assertIn(str(config), str(error))
                    else:
                        self.assertTrue(problems)

    def test_check_environment_cli(self):
        start = self.root / "work"
        start.mkdir()
        home = self.root / "home-cargo"
        home.mkdir()
        base = {"CARGO_HOME": str(home), "PATH": os.environ.get("PATH", "")}
        with patch.dict(os.environ, base, clear=True):
            status, out, _ = self.run_main("--check-environment", "--cargo-dir", start)
            self.assertEqual(status, 0)
            self.assertIn("OK build environment", out)
        for extra in ({"RUSTC_WRAPPER": "sccache"}, {"RUSTFLAGS": "--cfg duduclaw_test_root"},
                      {REFUSER.TEST_LICENSE_ISSUER_VARIABLE: "ab" * 32}):
            with self.subTest(extra=extra), patch.dict(os.environ, {**base, **extra}, clear=True):
                status, _, err = self.run_main("--check-environment", "--cargo-dir", start)
                self.assertEqual(status, 1)
                self.assertIn(next(iter(extra)), err)
        (start / ".cargo").mkdir()
        (start / ".cargo/config.toml").write_text("# duduclaw_test_root\n")
        with patch.dict(os.environ, base, clear=True):
            self.assertEqual(self.run_main("--check-environment", "--cargo-dir", start)[0], 1)

    def test_no_release_job_sets_a_rustc_wrapper(self):
        # The wrapper rule allows no exception because no release path sets one.
        for path in [*(ROOT / ".github/workflows").glob("*.yml"), ROOT / "scripts/release.sh",
                     ROOT / "container/Dockerfile.server", ROOT / ".cargo/config.toml"]:
            with self.subTest(path=path.name):
                text = path.read_text()
                for variable in REFUSER.RUSTC_VARIABLES:
                    self.assertNotRegex(text, rf"(?m)\b{variable}\s*[:=]")
                self.assertNotIn("sccache", text)

    # ---------------------------------------------------------------- the packager shares it

    @unittest.skipIf(PACKAGE is None, "the Linux packager imports fcntl")
    def test_packager_refuses_through_the_shared_helper(self):
        self.assertEqual(Path(PACKAGE.REFUSER.__file__).resolve(), SCRIPT)
        self.assertEqual(PACKAGE.TEST_LICENSE_MARKER, MARKER)
        self.assertIs(PACKAGE.enables_test_root, PACKAGE.REFUSER.enables_test_root)
        self.assertIs(PACKAGE.rustflag_tokens, PACKAGE.REFUSER.rustflag_tokens)
        for name in ("TEST_ROOT_CFG", "TEST_LICENSE_ISSUER_VARIABLE", "BOOTSTRAP_TEST_KEY_VARIABLE", "RUSTFLAGS_VARIABLES"):
            self.assertEqual(getattr(PACKAGE, name), getattr(PACKAGE.REFUSER, name))
        with self.assertRaisesRegex(ValueError, "TEST-licence build"):
            PACKAGE.refuse_test_license_build(io.BytesIO(gateway(True)).read, PACKAGE.GATEWAY_BINARY)
        PACKAGE.refuse_test_license_build(io.BytesIO(gateway(False)).read, PACKAGE.GATEWAY_BINARY)
        for environment in ({"RUSTC_WRAPPER": "sccache"}, {"RUSTC": "/opt/rustc"},
                            {"RUSTFLAGS": "--cfg duduclaw_test_root"}):
            with self.subTest(environment=environment), self.assertRaisesRegex(ValueError, next(iter(environment))):
                PACKAGE.refuse_test_root_environment(environment, TARGET)
        PACKAGE.refuse_test_root_environment({}, TARGET)
        # The probe binary is never a release binary.
        self.assertNotIn("duduclaw-license-marker-probe", PACKAGE.BIN_PACKAGES)
        for include in (True, False):
            command = " ".join(PACKAGE.build_command(TARGET, "cross", include))
            self.assertNotIn("marker-probe", command)
        manifest = (ROOT / "crates/duduclaw-gateway/Cargo.toml").read_text()
        block = re.search(r'\[\[bin\]\]\nname = "duduclaw-license-marker-probe"\n(?:[^\[\n][^\n]*\n)*', manifest)
        self.assertIsNotNone(block)
        self.assertIn('required-features = ["marker-probe"]', block[0])
        self.assertNotRegex(manifest, r"(?m)^default\s*=.*marker-probe")

    @unittest.skipIf(PACKAGE is None, "the Linux packager imports fcntl")
    def test_only_the_test_signer_path_accepts_the_test_archive_name(self):
        release_name = PACKAGE.TARGETS[TARGET][1] + ".tar.gz"
        test_name = PACKAGE.TARGETS[TARGET][1] + "-TEST.tar.gz"
        for name in (release_name, test_name):
            archive = self.tar(name, [("duduclaw", gateway(False))])
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            Path(str(archive) + ".sha256").write_text(f"{digest}  {name}\n")
        for name, test_archive, accepted in ((release_name, False, True), (release_name, True, True),
                                             (test_name, False, False), (test_name, True, True)):
            with self.subTest(name=name, test_archive=test_archive):
                stage = Path(tempfile.mkdtemp(dir=self.root))
                if accepted:
                    copy, _ = PACKAGE.verified_release_archive(self.root / name, TARGET, stage, test_archive)
                    self.assertEqual(copy.name, name)
                else:
                    with self.assertRaisesRegex(ValueError, "is not the x86_64 release archive"):
                        PACKAGE.verified_release_archive(self.root / name, TARGET, stage, test_archive)
        kit = (ROOT / "scripts/isolation-test-keys.sh").read_text()
        self.assertIn('test_name = name[:-len(".tar.gz")] + "-TEST.tar.gz"', kit)
        self.assertIn("copy = os.path.join(directory, test_name)", kit)


if __name__ == "__main__":
    unittest.main()
