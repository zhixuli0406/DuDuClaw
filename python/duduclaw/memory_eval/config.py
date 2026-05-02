"""
memory_eval/config.py
評測系統全域配置

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Optional


class QuestionType(Enum):
    SINGLE_HOP    = "single_hop"
    MULTI_HOP     = "multi_hop"
    TEMPORAL      = "temporal"
    SUMMARIZATION = "summarization"


@dataclass
class EvalConfig:
    """原生 KPI 評測配置"""
    # Retention Rate
    rr_observation_days:      list[int] = field(default_factory=lambda: [7, 30])
    rr_importance_threshold:  float     = 0.7
    rr_recall_threshold:      float     = 0.75
    rr_baseline_sample_size:  int       = 100

    # Retrieval Accuracy
    ra_k:                     int       = 5
    ra_query_sample_size:     int       = 200
    ra_relevance_threshold:   float     = 0.80
    ra_llm_judge_sample_rate: float     = 0.10

    # Temporal Consistency (P2)
    tc_use_llm_check:         bool      = True
    tc_llm_check_sample_size: int       = 50

    # Episodic Pressure Response (P2)
    epr_lookback_days:        int       = 7
    epr_target_compression:   float     = 3.0

    # 共用
    agent_id:                 str       = ""
    db_dsn:                   str       = ""     # PostgreSQL DSN


@dataclass
class LOCOMOConfig:
    """LOCOMO Benchmark 評測配置"""
    dataset_version:          str           = "v1.0.0"
    dataset_commit_hash:      str           = ""
    dataset_base_path:        str           = "/data/eval/locomo"
    dataset_version_file:     str           = "/data/eval/locomo/VERSION"
    dataset_dir:              str           = "/data/eval/locomo/v1"
    sample_size:              Optional[int] = None
    judge_model:              str           = "claude-3-5-sonnet-20241022"
    judge_temperature:        float         = 0.0
    memory_namespace_prefix:  str           = "locomo_eval"
    isolation_mode:           bool          = True
    timeout_per_individual:   int           = 120
    max_concurrent:           int           = 5

    def __post_init__(self) -> None:
        if not self.dataset_commit_hash:
            try:
                from .locomo_integrity_check import get_dataset_version
                info = get_dataset_version()
                self.dataset_commit_hash = info.get("commit_hash", "")
                self.dataset_version = info.get("version", self.dataset_version)
            except (FileNotFoundError, ImportError):
                pass
