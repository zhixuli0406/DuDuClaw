"""
WikiFilesSync — Wiki L0/L1 頁面 ↔ Anthropic Files API 同步工具
================================================================

這是 PoC 的第二個模組，示範如何在實際整合中維護
page_name → file_id 的映射表，並按需重新上傳。

生產環境整合時此邏輯將移至 Rust (wiki_files_cache.rs)，
但 Python 版本用於驗證 API 行為和快取策略。

策略
----
1. 以 SHA-256 hash 偵測內容變更（避免不必要的重新上傳）
2. File ID 持久化至 JSON cache 檔（~/.duduclaw/files_api_cache.json）
3. TTL 24h：超過時強制重新上傳（Files API 有保存期限限制）
4. 單一 bundle 策略：所有 L0+L1 頁面合併為一個 file（減少 file_id 管理複雜度）

使用方式
--------
  from wiki_files_sync import WikiFilesSync
  import anthropic

  client = anthropic.Anthropic(api_key="sk-ant-...")
  sync = WikiFilesSync(client, wiki_dir=Path("~/.duduclaw/shared/wiki"))
  file_id = sync.ensure_current()
  # → 若無快取或快取過期，自動上傳；否則直接回傳既有 file_id
"""

from __future__ import annotations

import hashlib
import json
import logging
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)

# Files API beta header
FILES_API_BETA = "files-api-2025-04-14"

# Cache TTL in seconds (24 hours)
CACHE_TTL_SECONDS = 24 * 60 * 60

# Default cache file location
DEFAULT_CACHE_PATH = Path.home() / ".duduclaw" / "files_api_cache.json"


# ---------------------------------------------------------------------------
# Cache types
# ---------------------------------------------------------------------------

@dataclass
class FilesApiCacheEntry:
    """Persistent cache entry for an uploaded wiki bundle."""
    file_id: str
    content_hash: str       # SHA-256 of the combined wiki content
    uploaded_at: float      # UNIX timestamp
    size_bytes: int
    page_count: int
    filename: str


# ---------------------------------------------------------------------------
# Sync class
# ---------------------------------------------------------------------------

class WikiFilesSync:
    """Manages the wiki bundle → Files API lifecycle.

    Responsibilities:
    - Detect content changes via SHA-256 hash
    - Upload when cache is missing, stale (>TTL), or content changed
    - Persist file_id → hash mapping to local JSON cache
    - Delete old files when re-uploading (Files API cleanup)
    """

    def __init__(
        self,
        client,
        wiki_dir: Optional[Path] = None,
        cache_path: Path = DEFAULT_CACHE_PATH,
        ttl_seconds: int = CACHE_TTL_SECONDS,
        bundle_filename: str = "wiki-knowledge-bundle.md",
    ) -> None:
        self._client = client
        self._wiki_dir = wiki_dir
        self._cache_path = cache_path
        self._ttl_seconds = ttl_seconds
        self._bundle_filename = bundle_filename

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def ensure_current(self) -> Optional[str]:
        """Return a valid file_id for the current wiki bundle.

        Uploads a new bundle if:
        - No cache entry exists
        - Cache entry is older than TTL
        - Wiki content has changed (hash mismatch)

        Returns None if the Files API is unavailable or upload fails.
        """
        pages = self._load_wiki_pages()
        if not pages:
            logger.warning("No L0/L1 wiki pages found — Files API injection unavailable")
            return None

        content = self._bundle_content(pages)
        current_hash = self._sha256(content)
        cache = self._load_cache()

        if cache is not None:
            age = time.time() - cache.uploaded_at
            if cache.content_hash == current_hash and age < self._ttl_seconds:
                logger.debug(
                    "Cache hit: file_id=%s age=%.0fh hash=%s...",
                    cache.file_id, age / 3600, current_hash[:8],
                )
                return cache.file_id
            elif cache.content_hash != current_hash:
                logger.info("Wiki content changed — re-uploading (hash %s → %s)", cache.content_hash[:8], current_hash[:8])
            else:
                logger.info("Cache TTL expired (%.0fh > %.0fh) — re-uploading", age / 3600, self._ttl_seconds / 3600)

        # Upload new bundle FIRST, then delete old (avoids losing file_id on upload failure)
        file_id = self._upload_bundle(content, current_hash, len(pages))
        if file_id is not None and cache is not None:
            self._delete_file(cache.file_id)
        return file_id

    def invalidate(self) -> None:
        """Force re-upload on next ensure_current() call."""
        if self._cache_path.exists():
            self._cache_path.unlink()
            logger.info("Cache invalidated: %s", self._cache_path)

    def current_file_id(self) -> Optional[str]:
        """Return the cached file_id without triggering an upload."""
        cache = self._load_cache()
        if cache is None:
            return None
        age = time.time() - cache.uploaded_at
        if age > self._ttl_seconds:
            return None
        return cache.file_id

    def stats(self) -> Optional[dict]:
        """Return current cache stats for monitoring."""
        cache = self._load_cache()
        if cache is None:
            return None
        age = time.time() - cache.uploaded_at
        return {
            "file_id": cache.file_id,
            "content_hash": cache.content_hash,
            "age_hours": round(age / 3600, 1),
            "ttl_hours": round(self._ttl_seconds / 3600, 1),
            "size_bytes": cache.size_bytes,
            "page_count": cache.page_count,
            "valid": age < self._ttl_seconds,
        }

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _load_wiki_pages(self) -> dict[str, str]:
        """Load L0/L1 pages from wiki directory."""
        from poc_files_api import load_wiki_pages
        return load_wiki_pages(self._wiki_dir)

    def _bundle_content(self, pages: dict[str, str]) -> str:
        """Produce deterministic bundle content from pages (sorted by filename)."""
        sections = []
        for filename in sorted(pages.keys()):
            sections.append(f"<!-- FILE: {filename} -->\n{pages[filename]}\n")
        return "\n---\n".join(sections)

    @staticmethod
    def _sha256(content: str) -> str:
        return hashlib.sha256(content.encode("utf-8")).hexdigest()

    def _load_cache(self) -> Optional[FilesApiCacheEntry]:
        if not self._cache_path.exists():
            return None
        try:
            data = json.loads(self._cache_path.read_text(encoding="utf-8"))
            return FilesApiCacheEntry(**data)
        except (json.JSONDecodeError, TypeError, KeyError) as e:
            logger.warning("Corrupted files API cache, ignoring: %s", e)
            return None
        except OSError as e:
            logger.error("Cannot read cache file %s: %s", self._cache_path, e)
            return None

    def _save_cache(self, entry: FilesApiCacheEntry) -> None:
        self._cache_path.parent.mkdir(parents=True, exist_ok=True)
        self._cache_path.write_text(
            json.dumps(asdict(entry), indent=2),
            encoding="utf-8",
        )
        logger.debug("Cache saved: %s", self._cache_path)

    def _upload_bundle(self, content: str, content_hash: str, page_count: int) -> Optional[str]:
        """Upload the wiki bundle to Files API and persist the cache entry."""
        content_bytes = content.encode("utf-8")
        logger.info(
            "Uploading wiki bundle: %s (%d bytes, %d pages)",
            self._bundle_filename, len(content_bytes), page_count,
        )
        try:
            response = self._client.beta.files.upload(
                file=(self._bundle_filename, content_bytes, "text/markdown"),
            )
            file_id = response.id
            entry = FilesApiCacheEntry(
                file_id=file_id,
                content_hash=content_hash,
                uploaded_at=time.time(),
                size_bytes=len(content_bytes),
                page_count=page_count,
                filename=self._bundle_filename,
            )
            self._save_cache(entry)
            logger.info("Upload complete: file_id=%s", file_id)
            return file_id
        except Exception as e:
            logger.error("Files API upload failed: %s", e)
            return None

    def _delete_file(self, file_id: str) -> None:
        """Delete a file from Files API (best-effort)."""
        try:
            self._client.beta.files.delete(file_id)
            logger.debug("Deleted old file: %s", file_id)
        except Exception as e:
            logger.warning("Could not delete file %s: %s", file_id, e)
