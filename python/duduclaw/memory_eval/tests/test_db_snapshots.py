"""
tests/test_db_snapshots.py
db/snapshots.py 的 unit tests（純 Python 邏輯，無需真實 DB）

覆蓋 _sha256_hash 等不依賴 asyncpg 的 utility 函式
"""
from __future__ import annotations

import hashlib

import pytest

from duduclaw.memory_eval.db.snapshots import _sha256_hash


# ---------------------------------------------------------------------------
# _sha256_hash unit tests
# ---------------------------------------------------------------------------

def test_sha256_hash_returns_hex_string():
    """_sha256_hash 回傳 64 位元 hex 字串"""
    result = _sha256_hash("hello world")
    assert isinstance(result, str)
    assert len(result) == 64
    assert all(c in "0123456789abcdef" for c in result)


def test_sha256_hash_correctness():
    """_sha256_hash 結果應與 hashlib.sha256 一致"""
    content = "用戶最愛的食物是拉麵"
    expected = hashlib.sha256(content.encode("utf-8")).hexdigest()
    assert _sha256_hash(content) == expected


def test_sha256_hash_empty_string():
    """空字串 sha256 應回傳 known hash"""
    expected = hashlib.sha256(b"").hexdigest()
    assert _sha256_hash("") == expected


def test_sha256_hash_deterministic():
    """相同輸入 → 相同輸出（無隨機性）"""
    content = "test content"
    assert _sha256_hash(content) == _sha256_hash(content)


def test_sha256_hash_different_inputs():
    """不同輸入 → 不同輸出"""
    assert _sha256_hash("abc") != _sha256_hash("def")


def test_sha256_hash_unicode():
    """Unicode 內容正確處理"""
    result = _sha256_hash("🍜🐕👤")
    assert len(result) == 64


def test_sha256_hash_long_content():
    """長字串（>1000 字元）也能正確 hash"""
    long_content = "A" * 10000
    result = _sha256_hash(long_content)
    expected = hashlib.sha256(long_content.encode("utf-8")).hexdigest()
    assert result == expected
