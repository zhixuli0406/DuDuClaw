"""Contract tests for the public wrapper; no private commercial checkout needed."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "build-distributor-brief.py"
BUILDER = '''from pathlib import Path
from enable_data import EXTRA
style = Path("style.css.part").read_text().replace("</style>", Path("extra.css").read_text() + "</style>")
body = Path("page-body.html").read_text()
Path("duduclaw-tech-brief.html").write_text("<title>Old title</title>\\n" + style + body + EXTRA)
'''
BODY = '''<header><div class="meta"><span>產品版本 v1.69.0（2026-10-04 發布）</span></div></header>
<p>v1.67.0 起棄用，v1.70.0 移除；v1.69.0 在預設權限下不能使用平台工具。</p>'''


class DistributorBriefTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "private source"
        self.source.mkdir()
        for name, content in {
            "build.py": BUILDER, "page-body.html": BODY,
            "style.css.part": "<style>body{color:black}</style>",
            "extra.css": "p{margin:0}", "enable_data.py": "EXTRA = '<footer>查核完成於 2026-10-04</footer>'",
        }.items():
            (self.source / name).write_text(content, encoding="utf-8")
        self.output = self.root / "output"

    def run_builder(self, version="1.69.1", *extra):
        return subprocess.run([sys.executable, str(SCRIPT), "--version", version,
                               "--source-dir", str(self.source), "--output-dir", str(self.output), *extra],
                              cwd=self.root, capture_output=True, text=True)

    def target(self, version="1.69.1"):
        return self.output / f"duduclaw-technology-brief-v{version}.html"

    def test_standalone_preserves_factual_baseline_without_source_writes(self):
        before = {p.name: p.read_bytes() for p in self.source.iterdir()}
        result = self.run_builder()
        self.assertEqual(result.returncode, 0, result.stderr)
        output = self.target().read_text()
        self.assertTrue(output.startswith('<!DOCTYPE html>\n<html lang="zh-Hant-TW">'))
        self.assertEqual(output.count("<title>"), 1)
        self.assertIn("<title>DuDuClaw 技術特色說明 v1.69.1</title>", output)
        self.assertIn('name="viewport"', output)
        self.assertIn("發版文件 v1.69.1；內容查核基準 v1.69.0，功能說明尚待逐項確認。", output)
        self.assertIn(BODY.split("\n")[1], output)
        self.assertIn("產品版本 v1.69.0（2026-10-04 發布）", output)
        self.assertIn("查核完成於 2026-10-04", output)
        self.assertLess(output.index("<style>"), output.index("</head>"))
        self.assertEqual(before, {p.name: p.read_bytes() for p in self.source.iterdir()})

    def test_same_baseline_has_no_notice_and_old_versions_remain(self):
        self.assertEqual(self.run_builder("1.69.0").returncode, 0)
        old = self.target("1.69.0").read_bytes()
        self.assertNotIn(b"release-baseline", old)
        self.assertEqual(self.run_builder().returncode, 0)
        self.assertEqual(self.target("1.69.0").read_bytes(), old)

    def test_identical_is_idempotent_and_different_requires_force(self):
        self.assertEqual(self.run_builder().returncode, 0)
        original = self.target().read_bytes()
        stat = self.target().stat()
        self.assertEqual(self.run_builder().returncode, 0)
        self.assertEqual(self.target().stat().st_mtime_ns, stat.st_mtime_ns)
        self.target().write_text("existing reviewed document")
        self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertEqual(self.target().read_text(), "existing reviewed document")
        self.assertEqual(self.run_builder("1.69.1", "--force").returncode, 0)
        self.assertEqual(self.target().read_bytes(), original)

    def test_absent_source_may_skip_but_incomplete_source_must_fail(self):
        absent = self.root / "absent"
        result = self.run_builder("1.69.1", "--source-dir", str(absent), "--skip-missing")
        self.assertEqual(result.returncode, 0)
        self.assertIn("SKIP", result.stdout)
        self.assertNotEqual(self.run_builder("1.69.1", "--source-dir", str(absent)).returncode, 0)
        for required in ("build.py", "page-body.html", "style.css.part", "extra.css", "enable_data.py"):
            file = self.source / required
            content = file.read_bytes()
            file.unlink()
            self.assertNotEqual(self.run_builder("1.69.1", "--skip-missing").returncode, 0, required)
            file.write_bytes(content)
        self.assertFalse(self.output.exists())

    def test_builder_failure_does_not_publish(self):
        (self.source / "build.py").write_text("raise RuntimeError('failed')")
        self.assertNotEqual(self.run_builder("1.69.1", "--skip-missing").returncode, 0)
        self.assertFalse(self.output.exists())

    def test_invalid_versions_fail_before_writing(self):
        for version in ("../escape", "1.2.3/../../escape", "v1.69.1", "1.2", "1.2.3-rc1", "01.2.3", "1.2.3\n"):
            with self.subTest(version=version):
                self.assertNotEqual(self.run_builder(version).returncode, 0)
        self.assertFalse(self.output.exists())

    def test_default_output_directory_is_source_parent(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--version", "1.69.1", "--source-dir", str(self.source)],
                                cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.source.parent / self.target().name).is_file())

    def test_missing_baseline_or_header_fails_without_publication(self):
        for body in ("<p>No version</p>", BODY.replace("產品版本 v1.69.0", "產品版本 v1.69.0-invalid"), BODY.replace("產品版本 v1.69.0", "產品版本 v01.69.0"), "<p>產品版本 v1.69.0</p>", '<header>產品版本 v1.69.0</header>'):
            (self.source / "page-body.html").write_text(body)
            self.assertNotEqual(self.run_builder().returncode, 0)
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
