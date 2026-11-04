import importlib.util
import contextlib
import io
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_file_location(
    "check_embedded_paths", Path(__file__).resolve().parents[1] / "check_embedded_paths.py"
)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


class EmbeddedPathsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=Path(__file__).parent)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        (self.root / "crates").mkdir()
        self.dockerfile = self.root / "Dockerfile"

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def run_check(self, copies="", stage=None):
        self.write("Dockerfile", "FROM base AS frontend\nCOPY scripts/ scripts/\n"
                   "FROM base AS rust-builder\nCOPY crates/ crates/\n" + copies +
                   "FROM base AS runtime\nCOPY scripts/ scripts/\n")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            args = [str(self.root), str(self.dockerfile)]
            if stage is not None:
                args.extend(["--stage", stage])
            code = check.main(args)
        return code, out.getvalue()

    def test_covered_external_include_passes(self):
        self.write("crates/cli/src/main.rs", 'include_str!(\n"../../../scripts/./googlechat/bootstrap.sh"\n);')
        self.write("scripts/googlechat/bootstrap.sh", "echo ok")
        code, out = self.run_check("COPY scripts/googlechat/ scripts/googlechat/\n")
        self.assertEqual(code, 0, out)
        self.assertIn("OK", out)

    def test_uncovered_include_fails_with_location(self):
        self.write("crates/cli/src/main.rs", '// source\ninclude_bytes!("../../../scripts/bootstrap.sh");')
        code, out = self.run_check("COPY --from=frontend scripts/ scripts/\nCOPY scripts-other/ scripts-other/\n")
        self.assertEqual(code, 1)
        self.assertIn("crates/cli/src/main.rs:2 -> scripts/bootstrap.sh", out)
        self.assertIn("add a COPY", out)

    def test_test_files_are_ignored(self):
        for name in ("tests/main.rs", "src/cli_tests.rs", "src/tests.rs", "src/tests_cli.rs"):
            self.write("crates/cli/" + name, 'include_str!("../../../missing");')
        self.assertEqual(self.run_check()[0], 0)

    def test_single_file_is_covered(self):
        self.write("crates/cli/src/main.rs", 'include_str!("../../../Cargo.toml");')
        self.write("Cargo.toml", "")
        self.assertEqual(self.run_check("COPY Cargo.toml Cargo.lock ./\n")[0], 0)

    def test_internal_include_needs_no_extra_copy(self):
        self.write("crates/cli/src/main.rs", 'include_str!("../assets/icon.svg");')
        self.assertEqual(self.run_check()[0], 0)

    def test_comments_are_ignored_and_raw_literals_are_checked(self):
        self.write("crates/cli/src/main.rs", '// include_str!("../../../missing");\n'
                   '/* include_bytes!("../../../missing"); */\n'
                   'include_str!(r#"../../../scripts/bootstrap.sh"#);')
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("main.rs:3 -> scripts/bootstrap.sh", out)
        self.assertNotIn("-> missing", out)

    def test_escaped_literal_is_checked(self):
        self.write("crates/cli/src/main.rs", r'include_str!("../../../scripts/\x62ootstrap.sh");')
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("main.rs:1 -> scripts/bootstrap.sh", out)

    def test_stage_selection(self):
        self.write("crates/cli/src/main.rs", 'include_str!("../../../scripts/bootstrap.sh");')
        for stage in ("frontend", "runtime", "FRONTEND"):
            with self.subTest(stage=stage):
                code, out = self.run_check(stage=stage)
                self.assertEqual(code, 0, out)
        self.assertEqual(self.run_check()[0], 1)

    def test_unknown_stage_fails(self):
        code, out = self.run_check(stage="unknown")
        self.assertEqual(code, 1)
        self.assertIn("Docker stage not found: unknown", out)

    def test_empty_stage_fails(self):
        self.write("Dockerfile", "FROM base AS rust-builder\nRUN true\n")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = check.main([str(self.root), str(self.dockerfile)])
        self.assertEqual(code, 1)
        self.assertIn("No COPY sources found in rust-builder stage", out.getvalue())

    def test_missing_inputs_fail(self):
        code, out = self.run_check()
        self.assertEqual(code, 0, out)
        self.dockerfile.unlink()
        for missing in ("Dockerfile", "crates"):
            if missing == "crates":
                (self.root / "crates").rmdir()
                self.write("Dockerfile", "FROM base AS rust-builder\nCOPY crates/ crates/\n")
            with self.subTest(missing=missing), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(check.main([str(self.root), str(self.dockerfile)]), 1)

    def test_inline_test_module_is_checked(self):
        self.write("crates/cli/src/main.rs", '#[cfg(test)]\nmod tests {\n'
                   'include_str!("../../../web/fixture.json");\n}')
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("main.rs:3 -> web/fixture.json", out)

    def test_unsupported_escape_fails(self):
        self.write("crates/cli/src/main.rs", r'include_str!("../../../scripts/\q.sh");')
        code, out = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("Unsupported escape", out)

    def test_real_dockerfiles(self):
        root = Path(__file__).resolve().parents[2]
        for name, stage in (("server", "rust-builder"), ("edition-smoke", "builder")):
            with self.subTest(dockerfile=name):
                result = subprocess.run(
                    [sys.executable, str(root / "scripts/check_embedded_paths.py"),
                     str(root), str(root / f"container/Dockerfile.{name}"), "--stage", stage],
                    capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("OK", result.stdout)


if __name__ == "__main__":
    unittest.main()
