"""
Unit tests for WikiFilesSync — wiki_files_sync.py
===================================================

All tests use unittest.mock to avoid real Anthropic API calls.
No ANTHROPIC_API_KEY required.

Run:
    cd research/files-api-wiki-poc
    python -m pytest tests/test_wiki_files_sync.py -v

Coverage target: 80%+ of wiki_files_sync.py
"""

from __future__ import annotations

import hashlib
import json
import sys
import time
import tempfile
import unittest
from pathlib import Path
from typing import Optional
from unittest.mock import MagicMock, patch

# Make parent directory importable (research/files-api-wiki-poc)
sys.path.insert(0, str(Path(__file__).parent.parent))

from wiki_files_sync import (
    WikiFilesSync,
    FilesApiCacheEntry,
    CACHE_TTL_SECONDS,
    DEFAULT_CACHE_PATH,
)


# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------

def _fresh_entry(
    file_id: str = "file_test123",
    content_hash: Optional[str] = None,
) -> FilesApiCacheEntry:
    """Return a FilesApiCacheEntry that is NOT expired (uploaded 1 minute ago)."""
    return FilesApiCacheEntry(
        file_id=file_id,
        content_hash=content_hash or ("ab12cd34" * 8),
        uploaded_at=time.time() - 60,   # 1 minute ago — well within 24h TTL
        size_bytes=1024,
        page_count=3,
        filename="wiki-knowledge-bundle.md",
    )


def _stale_entry(file_id: str = "file_stale") -> FilesApiCacheEntry:
    """Return a FilesApiCacheEntry that IS expired (uploaded 25 hours ago)."""
    return FilesApiCacheEntry(
        file_id=file_id,
        content_hash="ab12cd34" * 8,
        uploaded_at=time.time() - (CACHE_TTL_SECONDS + 3600),  # 25h ago
        size_bytes=512,
        page_count=2,
        filename="wiki-knowledge-bundle.md",
    )


def _mock_client(upload_file_id: str = "file_new") -> MagicMock:
    """Return a mock Anthropic client with stub Files API responses."""
    client = MagicMock()
    upload_resp = MagicMock()
    upload_resp.id = upload_file_id
    client.beta.files.upload.return_value = upload_resp
    client.beta.files.delete.return_value = None
    return client


# Minimal sample wiki pages for testing (same structure as production L0/L1 pages)
SAMPLE_PAGES: dict[str, str] = {
    "identity.md": "# Identity\nI am DuDuClaw agent.\n",
    "core.md": "# Core\nPlatform: v1.11.0\n",
}


# ---------------------------------------------------------------------------
# Helper: build the expected hash for SAMPLE_PAGES deterministically
# ---------------------------------------------------------------------------

def _expected_hash_for(pages: dict[str, str]) -> str:
    """Compute what WikiFilesSync._sha256(_bundle_content(pages)) should return."""
    sections = []
    for filename in sorted(pages.keys()):
        sections.append(f"<!-- FILE: {filename} -->\n{pages[filename]}\n")
    bundle = "\n---\n".join(sections)
    return hashlib.sha256(bundle.encode("utf-8")).hexdigest()


# ---------------------------------------------------------------------------
# FilesApiCacheEntry dataclass
# ---------------------------------------------------------------------------

class TestFilesApiCacheEntry(unittest.TestCase):
    """Verify FilesApiCacheEntry field values and serialisability."""

    def test_fields_preserved_after_asdict_roundtrip(self):
        from dataclasses import asdict
        entry = _fresh_entry("file_roundtrip")
        d = asdict(entry)
        restored = FilesApiCacheEntry(**d)
        self.assertEqual(restored.file_id, "file_roundtrip")
        self.assertEqual(restored.content_hash, entry.content_hash)
        self.assertAlmostEqual(restored.uploaded_at, entry.uploaded_at, places=3)

    def test_fresh_entry_age_within_ttl(self):
        entry = _fresh_entry()
        age = time.time() - entry.uploaded_at
        self.assertLess(age, CACHE_TTL_SECONDS)

    def test_stale_entry_age_exceeds_ttl(self):
        entry = _stale_entry()
        age = time.time() - entry.uploaded_at
        self.assertGreater(age, CACHE_TTL_SECONDS)


# ---------------------------------------------------------------------------
# _bundle_content — determinism & completeness
# ---------------------------------------------------------------------------

class TestBundleContent(unittest.TestCase):

    def _sync(self, tmp: Path) -> WikiFilesSync:
        return WikiFilesSync(client=MagicMock(), cache_path=tmp / "cache.json")

    def test_bundle_deterministic_regardless_of_dict_insertion_order(self):
        """Output must be identical no matter which order pages are inserted."""
        with tempfile.TemporaryDirectory() as td:
            sync = self._sync(Path(td))
            pages_a = {"z.md": "Z\n", "a.md": "A\n"}
            pages_b = {"a.md": "A\n", "z.md": "Z\n"}
            self.assertEqual(sync._bundle_content(pages_a), sync._bundle_content(pages_b))

    def test_bundle_contains_all_filenames(self):
        with tempfile.TemporaryDirectory() as td:
            sync = self._sync(Path(td))
            bundle = sync._bundle_content(SAMPLE_PAGES)
            for filename in SAMPLE_PAGES:
                self.assertIn(filename, bundle)

    def test_bundle_contains_all_page_content(self):
        with tempfile.TemporaryDirectory() as td:
            sync = self._sync(Path(td))
            bundle = sync._bundle_content(SAMPLE_PAGES)
            for content in SAMPLE_PAGES.values():
                self.assertIn(content.strip(), bundle)

    def test_pages_separated_by_horizontal_rule(self):
        with tempfile.TemporaryDirectory() as td:
            sync = self._sync(Path(td))
            bundle = sync._bundle_content(SAMPLE_PAGES)
            self.assertIn("---", bundle)

    def test_single_page_no_separator(self):
        with tempfile.TemporaryDirectory() as td:
            sync = self._sync(Path(td))
            bundle = sync._bundle_content({"only.md": "Only page\n"})
            # A single page should not have separator between pages
            self.assertNotIn("\n---\n", bundle)


# ---------------------------------------------------------------------------
# _sha256 — correctness & stability
# ---------------------------------------------------------------------------

class TestSha256(unittest.TestCase):

    def _sync(self) -> WikiFilesSync:
        return WikiFilesSync(client=MagicMock())

    def test_same_content_produces_same_hash(self):
        sync = self._sync()
        self.assertEqual(sync._sha256("hello world"), sync._sha256("hello world"))

    def test_different_content_produces_different_hash(self):
        sync = self._sync()
        self.assertNotEqual(sync._sha256("alpha"), sync._sha256("beta"))

    def test_hash_is_64_hex_chars(self):
        sync = self._sync()
        h = sync._sha256("arbitrary content")
        self.assertEqual(len(h), 64)
        self.assertRegex(h, r"^[0-9a-f]{64}$")

    def test_matches_stdlib_sha256(self):
        sync = self._sync()
        content = "DuDuClaw wiki bundle content"
        expected = hashlib.sha256(content.encode("utf-8")).hexdigest()
        self.assertEqual(sync._sha256(content), expected)

    def test_empty_string_has_known_hash(self):
        sync = self._sync()
        # SHA-256 of "" is a known constant
        self.assertEqual(
            sync._sha256(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )


# ---------------------------------------------------------------------------
# _load_cache / _save_cache
# ---------------------------------------------------------------------------

class TestPersistentCache(unittest.TestCase):

    def test_load_returns_none_for_missing_file(self):
        with tempfile.TemporaryDirectory() as td:
            sync = WikiFilesSync(
                client=MagicMock(),
                cache_path=Path(td) / "nonexistent.json",
            )
            self.assertIsNone(sync._load_cache())

    def test_save_then_load_roundtrip(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)

            entry = _fresh_entry("file_saved")
            sync._save_cache(entry)

            loaded = sync._load_cache()
            self.assertIsNotNone(loaded)
            self.assertEqual(loaded.file_id, "file_saved")
            self.assertEqual(loaded.content_hash, entry.content_hash)

    def test_load_returns_none_for_invalid_json(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "bad.json"
            cache_path.write_text("{ bad json !!!", encoding="utf-8")
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            self.assertIsNone(sync._load_cache())

    def test_load_returns_none_for_missing_required_fields(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "partial.json"
            cache_path.write_text(json.dumps({"file_id": "xyz"}), encoding="utf-8")
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            self.assertIsNone(sync._load_cache())

    def test_save_creates_parent_directories(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "deep" / "nested" / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry())
            self.assertTrue(cache_path.exists())

    def test_saved_cache_is_valid_json(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry("file_json_valid"))

            raw = cache_path.read_text(encoding="utf-8")
            parsed = json.loads(raw)   # must not raise
            self.assertEqual(parsed["file_id"], "file_json_valid")


# ---------------------------------------------------------------------------
# ensure_current — core business logic
# ---------------------------------------------------------------------------

class TestEnsureCurrent(unittest.TestCase):
    """Tests for WikiFilesSync.ensure_current() — the main public API."""

    def _sync(self, cache_path: Path, client: MagicMock) -> WikiFilesSync:
        return WikiFilesSync(client=client, cache_path=cache_path)

    def _patch_pages(self, pages: dict[str, str]):
        return patch.object(WikiFilesSync, "_load_wiki_pages", return_value=pages)

    # --- No pages available ---

    def test_returns_none_when_no_wiki_pages_found(self):
        """If no L0/L1 pages exist, ensure_current must return None without uploading."""
        with tempfile.TemporaryDirectory() as td:
            client = _mock_client()
            sync = self._sync(Path(td) / "cache.json", client)

            with self._patch_pages({}):
                result = sync.ensure_current()

            self.assertIsNone(result)
            client.beta.files.upload.assert_not_called()

    # --- Cold start (no cache) ---

    def test_uploads_on_cold_start_and_returns_file_id(self):
        """No cache → upload → return new file_id."""
        with tempfile.TemporaryDirectory() as td:
            client = _mock_client("file_cold_start")
            sync = self._sync(Path(td) / "cache.json", client)

            with self._patch_pages(SAMPLE_PAGES):
                result = sync.ensure_current()

            self.assertEqual(result, "file_cold_start")
            client.beta.files.upload.assert_called_once()

    def test_cold_start_persists_cache_entry(self):
        """After upload, the cache JSON must be written."""
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = _mock_client("file_persist")
            sync = self._sync(cache_path, client)

            with self._patch_pages(SAMPLE_PAGES):
                sync.ensure_current()

            self.assertTrue(cache_path.exists(), "Cache file must be written after upload")
            saved = sync._load_cache()
            self.assertIsNotNone(saved)
            self.assertEqual(saved.file_id, "file_persist")

    # --- Cache hit ---

    def test_returns_cached_file_id_without_upload_on_hit(self):
        """Fresh cache + same content → no upload, return existing file_id."""
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = _mock_client()
            sync = self._sync(cache_path, client)

            # Seed cache with the correct hash for SAMPLE_PAGES
            correct_hash = _expected_hash_for(SAMPLE_PAGES)
            sync._save_cache(_fresh_entry("file_cached", content_hash=correct_hash))

            with self._patch_pages(SAMPLE_PAGES):
                result = sync.ensure_current()

            self.assertEqual(result, "file_cached")
            client.beta.files.upload.assert_not_called()

    # --- Stale cache (TTL expired) ---

    def test_reuploads_on_expired_cache_ttl(self):
        """Cache older than 24h → delete old file, upload new."""
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = _mock_client("file_refreshed")
            sync = self._sync(cache_path, client)

            # Stale entry with correct hash (content unchanged, TTL expired)
            correct_hash = _expected_hash_for(SAMPLE_PAGES)
            stale = FilesApiCacheEntry(
                file_id="file_stale_ttl",
                content_hash=correct_hash,
                uploaded_at=time.time() - (CACHE_TTL_SECONDS + 3600),
                size_bytes=500,
                page_count=2,
                filename="wiki-knowledge-bundle.md",
            )
            sync._save_cache(stale)

            with self._patch_pages(SAMPLE_PAGES):
                result = sync.ensure_current()

            self.assertEqual(result, "file_refreshed")
            client.beta.files.upload.assert_called_once()
            client.beta.files.delete.assert_called_once_with("file_stale_ttl")

    # --- Content changed ---

    def test_reuploads_when_wiki_content_changes(self):
        """Fresh cache but hash mismatch → re-upload regardless of TTL."""
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = _mock_client("file_content_updated")
            sync = self._sync(cache_path, client)

            # Cache has WRONG hash (simulates wiki content change)
            old = _fresh_entry("file_old_content", content_hash="00000000" * 8)
            sync._save_cache(old)

            with self._patch_pages(SAMPLE_PAGES):
                result = sync.ensure_current()

            self.assertEqual(result, "file_content_updated")
            client.beta.files.upload.assert_called_once()
            client.beta.files.delete.assert_called_once_with("file_old_content")

    # --- Upload failure ---

    def test_returns_none_on_upload_error(self):
        """Upload failure → return None so caller can fall back to text injection."""
        with tempfile.TemporaryDirectory() as td:
            client = MagicMock()
            client.beta.files.upload.side_effect = Exception("503 Service Unavailable")
            sync = self._sync(Path(td) / "cache.json", client)

            with self._patch_pages(SAMPLE_PAGES):
                result = sync.ensure_current()

            self.assertIsNone(result)

    def test_does_not_delete_old_file_if_upload_fails(self):
        """If the upload fails, the old (still valid) file must NOT be deleted."""
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = MagicMock()
            client.beta.files.upload.side_effect = Exception("network failure")

            sync = self._sync(cache_path, client)
            old = _fresh_entry("file_should_survive", content_hash="wrong_hash")
            sync._save_cache(old)

            with self._patch_pages(SAMPLE_PAGES):
                sync.ensure_current()

            # Old file should NOT be deleted when upload fails
            client.beta.files.delete.assert_not_called()

    # --- Upload-before-delete ordering ---

    def test_upload_happens_before_delete(self):
        """Critical invariant: upload NEW file before deleting OLD file."""
        call_order: list[str] = []

        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = MagicMock()

            upload_resp = MagicMock()
            upload_resp.id = "file_new_order"

            def record_upload(*args, **kwargs):
                call_order.append("upload")
                return upload_resp

            def record_delete(*args, **kwargs):
                call_order.append("delete")

            client.beta.files.upload.side_effect = record_upload
            client.beta.files.delete.side_effect = record_delete

            sync = self._sync(cache_path, client)
            sync._save_cache(_fresh_entry("file_old_order", content_hash="old_hash"))

            with self._patch_pages(SAMPLE_PAGES):
                sync.ensure_current()

            self.assertEqual(
                call_order, ["upload", "delete"],
                "Upload MUST occur before delete to prevent data loss on failure",
            )


# ---------------------------------------------------------------------------
# invalidate
# ---------------------------------------------------------------------------

class TestInvalidate(unittest.TestCase):

    def test_invalidate_removes_cache_file(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry())
            self.assertTrue(cache_path.exists())

            sync.invalidate()
            self.assertFalse(cache_path.exists())

    def test_invalidate_does_not_raise_when_no_cache(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "no_cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            # Must not raise
            sync.invalidate()

    def test_ensure_current_uploads_after_invalidate(self):
        """After invalidation, ensure_current must upload again."""
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            client = _mock_client("file_post_invalidate")
            sync = WikiFilesSync(client=client, cache_path=cache_path)

            # Seed fresh cache
            correct_hash = _expected_hash_for(SAMPLE_PAGES)
            sync._save_cache(_fresh_entry("file_before_invalidate", content_hash=correct_hash))

            sync.invalidate()

            with patch.object(WikiFilesSync, "_load_wiki_pages", return_value=SAMPLE_PAGES):
                result = sync.ensure_current()

            self.assertEqual(result, "file_post_invalidate")
            client.beta.files.upload.assert_called_once()


# ---------------------------------------------------------------------------
# current_file_id
# ---------------------------------------------------------------------------

class TestCurrentFileId(unittest.TestCase):

    def test_returns_none_when_no_cache(self):
        with tempfile.TemporaryDirectory() as td:
            sync = WikiFilesSync(
                client=MagicMock(),
                cache_path=Path(td) / "missing.json",
            )
            self.assertIsNone(sync.current_file_id())

    def test_returns_file_id_when_fresh(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry("file_id_fresh"))
            self.assertEqual(sync.current_file_id(), "file_id_fresh")

    def test_returns_none_when_stale(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_stale_entry("file_id_stale"))
            self.assertIsNone(sync.current_file_id())


# ---------------------------------------------------------------------------
# stats
# ---------------------------------------------------------------------------

class TestStats(unittest.TestCase):

    def test_returns_none_when_no_cache(self):
        with tempfile.TemporaryDirectory() as td:
            sync = WikiFilesSync(
                client=MagicMock(),
                cache_path=Path(td) / "missing.json",
            )
            self.assertIsNone(sync.stats())

    def test_stats_has_required_keys(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry("file_stats"))

            s = sync.stats()
            self.assertIsNotNone(s)
            for key in ("file_id", "content_hash", "age_hours", "ttl_hours",
                        "size_bytes", "page_count", "valid"):
                self.assertIn(key, s, f"Missing key: {key}")

    def test_stats_valid_true_for_fresh_entry(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry("file_fresh_valid"))

            s = sync.stats()
            self.assertTrue(s["valid"])

    def test_stats_valid_false_for_stale_entry(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_stale_entry("file_stale_valid"))

            s = sync.stats()
            self.assertFalse(s["valid"])

    def test_stats_file_id_matches(self):
        with tempfile.TemporaryDirectory() as td:
            cache_path = Path(td) / "cache.json"
            sync = WikiFilesSync(client=MagicMock(), cache_path=cache_path)
            sync._save_cache(_fresh_entry("file_stats_id_check"))

            s = sync.stats()
            self.assertEqual(s["file_id"], "file_stats_id_check")


# ---------------------------------------------------------------------------
# _delete_file — error isolation
# ---------------------------------------------------------------------------

class TestDeleteFile(unittest.TestCase):

    def test_delete_failure_is_swallowed(self):
        """delete is best-effort; exceptions must not propagate."""
        client = MagicMock()
        client.beta.files.delete.side_effect = Exception("Connection refused")
        sync = WikiFilesSync(client=client)

        # Must not raise
        sync._delete_file("file_failing_delete")

    def test_delete_calls_api_with_correct_file_id(self):
        client = _mock_client()
        sync = WikiFilesSync(client=client)
        sync._delete_file("file_specific_id")
        client.beta.files.delete.assert_called_once_with("file_specific_id")


if __name__ == "__main__":
    unittest.main(verbosity=2)
