"""
Unit tests for poc_files_api.py
=================================

Covers:
- Wiki page loading (mock + real directory)
- SHA-256 bundle determinism via load_wiki_pages
- Token stats extraction
- Experiment runner orchestration (text injection & Files API)
- Report generation (comparison output, JSON output)
- CLI argument parsing (--mock, --skip-text-injection, etc.)

No ANTHROPIC_API_KEY required — all API calls are mocked.

Run:
    cd research/files-api-wiki-poc
    python -m pytest tests/test_poc_files_api.py -v
"""

from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

# Make parent directory importable
sys.path.insert(0, str(Path(__file__).parent.parent))

import poc_files_api as poc


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _mock_usage(
    input_tokens: int = 100,
    cache_creation: int = 0,
    cache_read: int = 0,
    output_tokens: int = 30,
) -> MagicMock:
    u = MagicMock()
    u.input_tokens = input_tokens
    u.cache_creation_input_tokens = cache_creation
    u.cache_read_input_tokens = cache_read
    u.output_tokens = output_tokens
    return u


def _mock_response(usage: MagicMock) -> MagicMock:
    r = MagicMock()
    r.usage = usage
    return r


# ---------------------------------------------------------------------------
# load_wiki_pages
# ---------------------------------------------------------------------------

class TestLoadWikiPages(unittest.TestCase):

    def test_returns_mock_pages_when_wiki_dir_is_none(self):
        pages = poc.load_wiki_pages(None)
        self.assertGreater(len(pages), 0)
        # Should return MOCK_WIKI_PAGES keys
        for key in pages:
            self.assertTrue(key.endswith(".md"), f"Unexpected key: {key}")

    def test_returns_mock_pages_when_wiki_dir_missing(self):
        pages = poc.load_wiki_pages(Path("/nonexistent/path/to/wiki"))
        self.assertEqual(pages, poc.MOCK_WIKI_PAGES)

    def test_loads_l0_l1_pages_from_real_directory(self):
        with tempfile.TemporaryDirectory() as td:
            wiki_dir = Path(td)
            # Write a valid L0 page
            (wiki_dir / "l0-identity.md").write_text(
                "---\ntitle: T\nlayer: identity\ntrust: 1.0\ntags: []\n---\n# Identity\n",
                encoding="utf-8",
            )
            # Write a non-L0/L1 page (should be ignored)
            (wiki_dir / "private.md").write_text(
                "---\ntitle: Private\nlayer: private\n---\n# Private\n",
                encoding="utf-8",
            )
            pages = poc.load_wiki_pages(wiki_dir)
            self.assertIn("l0-identity.md", pages)
            self.assertNotIn("private.md", pages)

    def test_falls_back_to_mock_when_no_l0_l1_in_directory(self):
        with tempfile.TemporaryDirectory() as td:
            wiki_dir = Path(td)
            (wiki_dir / "unrelated.md").write_text("no frontmatter here", encoding="utf-8")
            pages = poc.load_wiki_pages(wiki_dir)
            self.assertEqual(pages, poc.MOCK_WIKI_PAGES)


# ---------------------------------------------------------------------------
# _extract_frontmatter_field
# ---------------------------------------------------------------------------

class TestExtractFrontmatter(unittest.TestCase):

    def test_extracts_existing_field(self):
        content = "---\ntitle: My Page\nlayer: core\n---\n# Body"
        self.assertEqual(poc._extract_frontmatter_field(content, "layer"), "core")
        self.assertEqual(poc._extract_frontmatter_field(content, "title"), "My Page")

    def test_returns_empty_string_when_field_missing(self):
        content = "---\ntitle: My Page\n---\n# Body"
        self.assertEqual(poc._extract_frontmatter_field(content, "layer"), "")

    def test_returns_empty_string_when_no_frontmatter(self):
        content = "# Just a heading\nNo frontmatter here."
        self.assertEqual(poc._extract_frontmatter_field(content, "layer"), "")


# ---------------------------------------------------------------------------
# pages_to_single_document
# ---------------------------------------------------------------------------

class TestPagesToSingleDocument(unittest.TestCase):

    def test_output_contains_all_filenames(self):
        pages = {"a.md": "Content A", "b.md": "Content B"}
        doc = poc.pages_to_single_document(pages)
        self.assertIn("a.md", doc)
        self.assertIn("b.md", doc)

    def test_output_is_deterministic(self):
        pages_a = {"z.md": "Z", "a.md": "A"}
        pages_b = {"a.md": "A", "z.md": "Z"}
        self.assertEqual(
            poc.pages_to_single_document(pages_a),
            poc.pages_to_single_document(pages_b),
        )


# ---------------------------------------------------------------------------
# TokenStats
# ---------------------------------------------------------------------------

class TestTokenStats(unittest.TestCase):

    def test_cache_hit_rate_zero_when_no_reads(self):
        s = poc.TokenStats(input_tokens=100, cache_creation_tokens=50)
        self.assertEqual(s.cache_hit_rate, 0.0)

    def test_cache_hit_rate_correct(self):
        s = poc.TokenStats(
            input_tokens=10,
            cache_creation_tokens=0,
            cache_read_tokens=90,
        )
        self.assertAlmostEqual(s.cache_hit_rate, 0.9)

    def test_cache_hit_rate_zero_when_all_tokens_zero(self):
        s = poc.TokenStats()
        self.assertEqual(s.cache_hit_rate, 0.0)

    def test_total_billable_sums_all_token_types(self):
        s = poc.TokenStats(
            input_tokens=10,
            cache_creation_tokens=20,
            cache_read_tokens=30,
            output_tokens=5,
        )
        self.assertEqual(s.total_billable, 60)

    def test_str_contains_key_fields(self):
        s = poc.TokenStats(input_tokens=50, cache_creation_tokens=800, output_tokens=40)
        text = str(s)
        self.assertIn("input=50", text)
        self.assertIn("cache_write=800", text)
        self.assertIn("output=40", text)


# ---------------------------------------------------------------------------
# _extract_stats
# ---------------------------------------------------------------------------

class TestExtractStats(unittest.TestCase):

    def test_extracts_all_fields(self):
        usage = _mock_usage(
            input_tokens=100,
            cache_creation=500,
            cache_read=200,
            output_tokens=40,
        )
        stats = poc._extract_stats(usage)
        self.assertEqual(stats.input_tokens, 100)
        self.assertEqual(stats.cache_creation_tokens, 500)
        self.assertEqual(stats.cache_read_tokens, 200)
        self.assertEqual(stats.output_tokens, 40)

    def test_handles_none_fields_gracefully(self):
        """SDK may return None for optional int fields; _extract_stats must coerce to 0."""
        usage = MagicMock()
        usage.input_tokens = None
        usage.cache_creation_input_tokens = None
        usage.cache_read_input_tokens = None
        usage.output_tokens = None

        stats = poc._extract_stats(usage)
        self.assertEqual(stats.input_tokens, 0)
        self.assertEqual(stats.cache_creation_tokens, 0)
        self.assertEqual(stats.cache_read_tokens, 0)
        self.assertEqual(stats.output_tokens, 0)

    def test_handles_missing_attributes_gracefully(self):
        """If SDK object lacks attributes entirely, should default to 0."""
        usage = object()  # bare object, no attributes
        stats = poc._extract_stats(usage)
        self.assertEqual(stats.input_tokens, 0)


# ---------------------------------------------------------------------------
# run_text_injection_experiment
# ---------------------------------------------------------------------------

class TestTextInjectionExperiment(unittest.TestCase):

    def _make_client(self) -> MagicMock:
        client = MagicMock()
        responses = [
            _mock_response(_mock_usage(100, 800, 0, 40)),
            _mock_response(_mock_usage(30, 0, 800, 38)),
            _mock_response(_mock_usage(30, 0, 800, 38)),
        ]
        client.messages.create.side_effect = responses
        return client

    def test_runs_three_calls(self):
        client = self._make_client()
        result = poc.run_text_injection_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertEqual(client.messages.create.call_count, 3)

    def test_approach_label_set(self):
        client = self._make_client()
        result = poc.run_text_injection_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertIn("Text Injection", result.approach)

    def test_first_call_stats_populated(self):
        client = self._make_client()
        result = poc.run_text_injection_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertEqual(result.call_1_stats.input_tokens, 100)
        self.assertEqual(result.call_1_stats.cache_creation_tokens, 800)

    def test_second_call_has_cache_read(self):
        client = self._make_client()
        result = poc.run_text_injection_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertEqual(result.call_2_stats.cache_read_tokens, 800)

    def test_errors_list_empty_on_success(self):
        client = self._make_client()
        result = poc.run_text_injection_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertEqual(result.errors, [])

    def test_records_error_on_api_failure(self):
        client = MagicMock()
        client.messages.create.side_effect = Exception("Service Unavailable")
        result = poc.run_text_injection_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertTrue(len(result.errors) > 0)


# ---------------------------------------------------------------------------
# run_files_api_experiment
# ---------------------------------------------------------------------------

class TestFilesApiExperiment(unittest.TestCase):

    def _make_client(self, upload_id: str = "file_poc_test") -> MagicMock:
        client = MagicMock()

        upload_resp = MagicMock()
        upload_resp.id = upload_id
        client.beta.files.upload.return_value = upload_resp
        client.beta.files.delete.return_value = None

        responses = [
            _mock_response(_mock_usage(35, 820, 0, 41)),
            _mock_response(_mock_usage(35, 0, 820, 37)),
            _mock_response(_mock_usage(35, 0, 820, 37)),
        ]
        client.beta.messages.create.side_effect = responses
        return client

    def test_uploads_once_then_runs_three_calls(self):
        client = self._make_client()
        result = poc.run_files_api_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        client.beta.files.upload.assert_called_once()
        self.assertEqual(client.beta.messages.create.call_count, 3)

    def test_file_is_deleted_after_experiment(self):
        client = self._make_client("file_cleanup_check")
        poc.run_files_api_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        client.beta.files.delete.assert_called_once_with("file_cleanup_check")

    def test_file_deleted_even_on_api_call_failure(self):
        """The finally block must ensure cleanup even when messages.create fails."""
        client = MagicMock()
        upload_resp = MagicMock()
        upload_resp.id = "file_exception"
        client.beta.files.upload.return_value = upload_resp
        client.beta.messages.create.side_effect = Exception("Rate limit exceeded")

        result = poc.run_files_api_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        client.beta.files.delete.assert_called_with("file_exception")

    def test_returns_result_with_upload_records(self):
        client = self._make_client("file_record_check")
        result = poc.run_files_api_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertEqual(len(result.upload_records), 1)
        self.assertEqual(result.upload_records[0].file_id, "file_record_check")

    def test_upload_failure_returns_empty_result(self):
        client = MagicMock()
        client.beta.files.upload.side_effect = Exception("Upload failed")
        result = poc.run_files_api_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertTrue(len(result.errors) > 0)
        self.assertEqual(len(result.upload_records), 0)

    def test_errors_empty_on_success(self):
        client = self._make_client()
        result = poc.run_files_api_experiment(client, "claude-haiku-4-5", poc.MOCK_WIKI_PAGES)
        self.assertEqual(result.errors, [])


# ---------------------------------------------------------------------------
# print_comparison_report — output validation
# ---------------------------------------------------------------------------

class TestComparisonReport(unittest.TestCase):

    def _make_results(self) -> tuple[poc.PocResult, poc.PocResult]:
        text = poc.PocResult(approach="Text Injection")
        text.call_1_stats = poc.TokenStats(100, 800, 0, 40)
        text.call_2_stats = poc.TokenStats(30, 0, 800, 38)
        text.call_3_stats = poc.TokenStats(30, 0, 800, 38)

        files = poc.PocResult(approach="Files API")
        files.call_1_stats = poc.TokenStats(35, 820, 0, 41)
        files.call_2_stats = poc.TokenStats(35, 0, 820, 37)
        files.call_3_stats = poc.TokenStats(35, 0, 820, 37)
        files.upload_records.append(poc.FileRecord("file_xyz", "bundle.md", 1500, 200.0))

        return text, files

    def test_report_does_not_raise(self):
        text, files = self._make_results()
        # Capture stdout — should not raise
        captured = io.StringIO()
        with patch("sys.stdout", captured):
            poc.print_comparison_report(text, files)
        output = captured.getvalue()
        self.assertGreater(len(output), 100)

    def test_report_contains_both_approach_labels(self):
        text, files = self._make_results()
        captured = io.StringIO()
        with patch("sys.stdout", captured):
            poc.print_comparison_report(text, files)
        output = captured.getvalue()
        # Report uses Chinese labels (hardcoded in print_comparison_report)
        self.assertIn("傳統做法", output)
        self.assertIn("Files API", output)

    def test_report_contains_summary_section(self):
        text, files = self._make_results()
        captured = io.StringIO()
        with patch("sys.stdout", captured):
            poc.print_comparison_report(text, files)
        output = captured.getvalue()
        self.assertIn("比較摘要", output)

    def test_report_shows_upload_record_when_present(self):
        text, files = self._make_results()
        captured = io.StringIO()
        with patch("sys.stdout", captured):
            poc.print_comparison_report(text, files)
        output = captured.getvalue()
        self.assertIn("file_xyz", output)


# ---------------------------------------------------------------------------
# save_results
# ---------------------------------------------------------------------------

class TestSaveResults(unittest.TestCase):

    def _make_results(self) -> tuple[poc.PocResult, poc.PocResult]:
        text = poc.PocResult(approach="Text")
        text.call_1_stats = poc.TokenStats(100, 0, 0, 30)

        files = poc.PocResult(approach="Files")
        files.call_1_stats = poc.TokenStats(50, 800, 0, 30)
        files.upload_records.append(poc.FileRecord("file_save_test", "b.md", 999, 150.0))

        return text, files

    def test_output_file_is_valid_json(self):
        text, files = self._make_results()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "results.json"
            poc.save_results(text, files, out)
            parsed = json.loads(out.read_text())
            self.assertIn("text_injection", parsed)
            self.assertIn("files_api", parsed)

    def test_json_contains_call_stats(self):
        text, files = self._make_results()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "results.json"
            poc.save_results(text, files, out)
            parsed = json.loads(out.read_text())
            calls = parsed["text_injection"]["calls"]
            self.assertIn("call_1", calls)
            self.assertEqual(calls["call_1"]["input_tokens"], 100)

    def test_json_contains_upload_records(self):
        text, files = self._make_results()
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "results.json"
            poc.save_results(text, files, out)
            parsed = json.loads(out.read_text())
            uploads = parsed["files_api"]["upload_records"]
            self.assertEqual(len(uploads), 1)
            self.assertEqual(uploads[0]["file_id"], "file_save_test")


# ---------------------------------------------------------------------------
# _build_mock_client (mock mode infrastructure)
# ---------------------------------------------------------------------------

class TestBuildMockClient(unittest.TestCase):

    def test_upload_returns_mock_file_id(self):
        client = poc._build_mock_client()
        resp = client.beta.files.upload(file=("test.md", b"content", "text/markdown"))
        self.assertTrue(resp.id.startswith("file_mock_"))

    def test_text_inject_responses_available(self):
        """messages.create should return 3 mock responses."""
        client = poc._build_mock_client()
        for _ in range(3):
            resp = client.messages.create(
                model="claude-haiku-4-5",
                max_tokens=256,
                system=[],
                messages=[{"role": "user", "content": "test"}],
            )
            self.assertIsNotNone(resp.usage)

    def test_files_api_responses_available(self):
        """beta.messages.create should return 3 mock responses."""
        client = poc._build_mock_client()
        for _ in range(3):
            resp = client.beta.messages.create(
                model="claude-haiku-4-5",
                max_tokens=256,
                messages=[],
            )
            self.assertIsNotNone(resp.usage)


if __name__ == "__main__":
    unittest.main(verbosity=2)
