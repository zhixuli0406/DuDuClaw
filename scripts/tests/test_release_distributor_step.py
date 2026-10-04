"""Exercise the actual pre-tag shell block without invoking a release."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RELEASE = (ROOT / "scripts/release.sh").read_text()
START = RELEASE.index("# --- Distributor technical brief")
END = RELEASE.index("# --- Git commit + tag ---", START)
BLOCK = RELEASE[START:END]


class ReleaseDistributorStepTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copyfile(ROOT / "scripts/build-distributor-brief.py", scripts / "build-distributor-brief.py")
        self.source = self.root / "commercial/marketing/distributor/technology-brief-v1.67-src"

    def run_step(self):
        env = dict(os.environ, NEW_VERSION="1.70.0")
        return subprocess.run(
            ["bash", "-eu", "-c", BLOCK + '\nprintf "REACHED_COMMIT_STAGE\\n"\n',
             str(self.root / "scripts/release.sh")],
            cwd=self.root, env=env, capture_output=True, text=True,
        )

    def test_absent_private_source_skips_and_continues(self):
        result = self.run_step()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("SKIP", result.stdout.upper())
        self.assertIn("REACHED_COMMIT_STAGE", result.stdout)

    def test_incomplete_source_stops_before_commit(self):
        self.source.mkdir(parents=True)
        result = self.run_step()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("REACHED_COMMIT_STAGE", result.stdout)
        self.assertIn("no release commit or tag", result.stdout)

    def test_valid_source_uses_new_version_before_commit(self):
        self.source.mkdir(parents=True)
        (self.source / "page-body.html").write_text(
            '<header><div class="meta"><span>產品版本 v1.69.0（2026-10-04 發布）</span></div></header>'
        )
        for name in ("enable_data.py", "style.css.part", "extra.css"):
            (self.source / name).write_text("")
        (self.source / "build.py").write_text(
            'from pathlib import Path\n'
            'Path("duduclaw-tech-brief.html").write_text('
            '\'<title>Brief</title>\\n\' + Path("page-body.html").read_text())\n'
        )
        result = self.run_step()
        self.assertEqual(result.returncode, 0, result.stderr)
        output = self.source.parent / "duduclaw-technology-brief-v1.70.0.html"
        self.assertTrue(output.is_file())
        self.assertIn("1.69.0", output.read_text())
        self.assertIn("REACHED_COMMIT_STAGE", result.stdout)

    def test_hook_follows_checks_and_precedes_commit(self):
        self.assertLess(RELEASE.index("cargo check --workspace"), START)
        self.assertLess(RELEASE.index("cargo check --manifest-path"), START)
        self.assertLess(END, RELEASE.index('git commit -m "chore: bump'))
        dry_run = RELEASE[RELEASE.index("if $DRY_RUN; then"):RELEASE.index("# --- Bump every")]
        self.assertNotIn("build-distributor-brief.py", dry_run)
        self.assertIn("duduclaw-technology-brief-v$NEW_VERSION.html", dry_run)


if __name__ == "__main__":
    unittest.main()
