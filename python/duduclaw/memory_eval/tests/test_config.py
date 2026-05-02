"""
tests/test_config.py
LOCOMOConfig.__post_init__ + EvalConfig + QuestionType — unit tests

覆蓋目標：config.py lines 66-73（__post_init__ 所有分支）
"""
from __future__ import annotations

from unittest.mock import patch

from duduclaw.memory_eval.config import EvalConfig, LOCOMOConfig, QuestionType


# ---------------------------------------------------------------------------
# QuestionType enum
# ---------------------------------------------------------------------------

def test_question_type_values():
    """QuestionType 列舉值正確"""
    assert QuestionType.SINGLE_HOP.value == "single_hop"
    assert QuestionType.MULTI_HOP.value == "multi_hop"
    assert QuestionType.TEMPORAL.value == "temporal"
    assert QuestionType.SUMMARIZATION.value == "summarization"


# ---------------------------------------------------------------------------
# EvalConfig defaults
# ---------------------------------------------------------------------------

def test_eval_config_defaults():
    """EvalConfig 預設值應可正常建立"""
    cfg = EvalConfig()
    assert cfg.rr_observation_days == [7, 30]
    assert cfg.rr_importance_threshold == 0.7
    assert cfg.ra_k == 5
    assert cfg.agent_id == ""


# ---------------------------------------------------------------------------
# LOCOMOConfig.__post_init__ — line 66: early-exit when hash already set
# ---------------------------------------------------------------------------

def test_locomo_config_skips_lookup_when_hash_provided():
    """line 66: dataset_commit_hash 非空 → 不呼叫 get_dataset_version"""
    with patch(
        "duduclaw.memory_eval.locomo_integrity_check.get_dataset_version"
    ) as mock_fn:
        cfg = LOCOMOConfig(dataset_commit_hash="abc123")

    mock_fn.assert_not_called()
    assert cfg.dataset_commit_hash == "abc123"


# ---------------------------------------------------------------------------
# LOCOMOConfig.__post_init__ — lines 68-71: success path
# ---------------------------------------------------------------------------

def test_locomo_config_populates_hash_from_get_dataset_version():
    """lines 68-71: hash 為空 + get_dataset_version 成功 → 填入 commit_hash & version"""
    fake_info = {"commit_hash": "deadbeef", "version": "v2.0.0"}

    with patch(
        "duduclaw.memory_eval.locomo_integrity_check.get_dataset_version",
        return_value=fake_info,
    ):
        cfg = LOCOMOConfig()  # dataset_commit_hash 預設 ""

    assert cfg.dataset_commit_hash == "deadbeef"
    assert cfg.dataset_version == "v2.0.0"


def test_locomo_config_keeps_default_version_when_key_missing():
    """lines 68-71: get_dataset_version 回傳不含 version 鍵 → 保留原預設版本"""
    with patch(
        "duduclaw.memory_eval.locomo_integrity_check.get_dataset_version",
        return_value={"commit_hash": "cafe0001"},
    ):
        cfg = LOCOMOConfig()

    assert cfg.dataset_commit_hash == "cafe0001"
    assert cfg.dataset_version == "v1.0.0"  # 預設值不變


# ---------------------------------------------------------------------------
# LOCOMOConfig.__post_init__ — lines 72-73: FileNotFoundError suppressed
# ---------------------------------------------------------------------------

def test_locomo_config_suppresses_file_not_found_error():
    """lines 72-73: get_dataset_version 拋出 FileNotFoundError → 靜默忽略，hash 維持空"""
    with patch(
        "duduclaw.memory_eval.locomo_integrity_check.get_dataset_version",
        side_effect=FileNotFoundError("VERSION file missing"),
    ):
        cfg = LOCOMOConfig()

    assert cfg.dataset_commit_hash == ""


def test_locomo_config_suppresses_import_error():
    """lines 72-73: get_dataset_version 拋出 ImportError → 靜默忽略，hash 維持空"""
    with patch(
        "duduclaw.memory_eval.locomo_integrity_check.get_dataset_version",
        side_effect=ImportError("module not found"),
    ):
        cfg = LOCOMOConfig()

    assert cfg.dataset_commit_hash == ""
