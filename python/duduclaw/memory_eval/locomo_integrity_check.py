"""
memory_eval/locomo_integrity_check.py
LOCOMO dataset 版本驗證工具

功能：
  - 讀取 /data/eval/locomo/VERSION 文件，回傳 dataset 版本與 commit hash
  - 提供 dataset 完整性檢查入口（W22 延伸用）

VERSION 文件格式（TOML-lite，一行一鍵值）：
    version=v1.0.0
    commit_hash=abc1234def5678901234567890abcdef12345678
    downloaded_at=2026-04-28T03:00:00+00:00

W21 Sprint — ENG-MEMORY
任務：0b5c478e-729d-46f7-9408-a2f281c605f4
"""
from __future__ import annotations

import logging
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)

DEFAULT_VERSION_FILE = Path("/data/eval/locomo/VERSION")


def get_dataset_version(
    version_file: Optional[Path] = None,
) -> dict[str, str]:
    """
    讀取 LOCOMO dataset 版本資訊。

    Args:
        version_file: VERSION 文件路徑，預設為 DEFAULT_VERSION_FILE

    Returns:
        dict with keys:
          - version:        str（e.g. "v1.0.0"）
          - commit_hash:    str（e.g. "abc1234def56..."）
          - downloaded_at:  str（ISO8601，可能為空）

    Raises:
        FileNotFoundError: VERSION 文件不存在
        ValueError: 文件格式不合法（無法解析任何 key=value）
    """
    path = version_file or DEFAULT_VERSION_FILE

    if not Path(path).exists():
        raise FileNotFoundError(
            f"LOCOMO VERSION file not found: {path}\n"
            "Run the dataset download script first: "
            "scripts/download_locomo_dataset.sh"
        )

    result: dict[str, str] = {
        "version": "",
        "commit_hash": "",
        "downloaded_at": "",
    }

    parsed_any = False
    with open(path, encoding="utf-8") as f:
        for lineno, raw_line in enumerate(f, 1):
            line = raw_line.strip()

            # 跳過空行與注釋
            if not line or line.startswith("#"):
                continue

            if "=" not in line:
                logger.debug(
                    "locomo_integrity_check: skipping non-kv line %d: %r",
                    lineno, line,
                )
                continue

            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip()

            if key in result:
                result[key] = value
                parsed_any = True
            else:
                logger.debug(
                    "locomo_integrity_check: unknown key %r at line %d",
                    key, lineno,
                )

    if not parsed_any:
        raise ValueError(
            f"LOCOMO VERSION file at {path} contains no valid key=value pairs. "
            "Expected keys: version, commit_hash, downloaded_at"
        )

    logger.info(
        "locomo_integrity_check: version=%s commit_hash=%s...",
        result["version"],
        result["commit_hash"][:8] if result["commit_hash"] else "(none)",
    )
    return result


def verify_dataset_path(dataset_dir: Optional[str] = None) -> bool:
    """
    快速驗證 LOCOMO dataset 目錄是否存在並有基本結構。

    Args:
        dataset_dir: dataset 目錄路徑，預設 /data/eval/locomo/v1

    Returns:
        True = 目錄存在且非空；False = 不存在或無法存取
    """
    base = Path(dataset_dir or "/data/eval/locomo/v1")

    if not base.exists():
        logger.warning(
            "locomo_integrity_check: dataset dir not found: %s", base
        )
        return False

    if not base.is_dir():
        logger.warning(
            "locomo_integrity_check: path is not a directory: %s", base
        )
        return False

    # 至少要有一個 .json 或 .jsonl 文件
    has_data = any(base.glob("*.json")) or any(base.glob("*.jsonl"))
    if not has_data:
        logger.warning(
            "locomo_integrity_check: dataset dir is empty (no .json/.jsonl): %s",
            base,
        )
        return False

    return True
