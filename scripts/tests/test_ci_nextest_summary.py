"""Keep retried passes visible and preserve each nextest suite's report."""

import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ci_nextest_summary


class NextestSummaryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.report = self.root / "junit.xml"

    def test_flaky_passes_and_failures_are_reported(self):
        self.report.write_text('''<testsuites><testsuite name="suite">
          <testcase classname="binary" name="retried_failure"><flakyFailure/></testcase>
          <testcase classname="binary" name="retried_error"><flakyError/></testcase>
          <testcase classname="binary" name="failed"><failure/></testcase>
          <testcase classname="binary" name="errored"><error/></testcase>
          <testcase classname="binary" name="passed"/>
        </testsuite></testsuites>''')
        summary = ci_nextest_summary.summarize(self.report)
        for name in ("retried_failure", "retried_error"):
            self.assertIn(f"**FLAKY** <code>binary::{name}</code>", summary)
        for name in ("failed", "errored"):
            self.assertIn(f"**FAILED** <code>binary::{name}</code>", summary)
        self.assertNotIn("binary::passed", summary)

    def test_clean_report_and_escaped_names(self):
        self.report.write_text('<testsuite><testcase classname="ok" name="passed"/></testsuite>')
        self.assertIn("No flaky or failed tests.", ci_nextest_summary.summarize(self.report))
        self.report.write_text('<testsuite><testcase classname="&lt;script&gt;" name="&amp;bad"><failure/></testcase></testsuite>')
        result = ci_nextest_summary.summarize(self.report)
        self.assertIn("&lt;script&gt;::&amp;bad", result)
        self.assertNotIn("<script>", result)

    def test_missing_and_malformed_reports_are_visible(self):
        self.assertIn("JUnit report unavailable:", ci_nextest_summary.summarize(self.report))
        self.report.write_text("<broken")
        self.assertIn("JUnit report unavailable:", ci_nextest_summary.summarize(self.report))

    def test_each_report_is_saved_and_summary_is_appended(self):
        summary_path = self.root / "summary.md"
        summary_path.write_text("Earlier step\n")
        for name, tag in (("workspace", "flakyFailure"), ("gateway", "failure")):
            self.report.write_text(f'<testsuite><testcase name="{name}"><{tag}/></testcase></testsuite>')
            saved = self.root / "reports" / f"{name}.xml"
            with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary_path)}), \
                    patch.object(sys, "argv", ["ci_nextest_summary.py", str(self.report), str(saved)]):
                ci_nextest_summary.main()
            self.assertFalse(self.report.exists())
            self.assertIn(name, saved.read_text())
        summary = summary_path.read_text()
        self.assertTrue(summary.startswith("Earlier step\n"))
        self.assertIn("**FLAKY** <code>::workspace</code>", summary)
        self.assertIn("**FAILED** <code>::gateway</code>", summary)
        self.assertTrue((self.root / "reports/workspace.xml").is_file())
        self.assertTrue((self.root / "reports/gateway.xml").is_file())

    def test_main_records_missing_report_without_raising(self):
        summary = self.root / "summary.md"
        saved = self.root / "reports/workspace.xml"
        with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}), \
                patch.object(sys, "argv", ["ci_nextest_summary.py", str(self.report), str(saved)]):
            ci_nextest_summary.main()
        self.assertIn("JUnit report unavailable:", summary.read_text())


if __name__ == "__main__":
    unittest.main()
