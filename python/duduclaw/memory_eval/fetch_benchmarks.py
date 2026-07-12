"""
memory_eval/fetch_benchmarks.py
LongMemEval-V2 / PersonaMem-v2 資料集下載＋轉檔工具

設計原則（誠實紀律）：
  - **不在 build 期下載大檔**：僅在操作者顯式執行本工具時才連外。
  - **無網路 / 無 datasets 套件 / 無憑證 → 誠實 fail**：印出清楚的取得指令，
    絕不捏造假資料集，也不靜默產生空/垃圾檔。
  - **schema 防呆**：HF 欄位名未經本地驗證，轉檔採「候選欄位名 best-effort 映射」，
    映射不到必需欄位就中止並印出實際欄位，同時永遠先落地一份 `*.raw.jsonl` 逐列原檔，
    確保下載成果不遺失。

轉出格式對齊各 loader：
  LongMemEval → longmemeval_v2.py 的 {question_id, question, ability, evidence_memory_ids, haystack, answer}
  PersonaMem  → personamem_v2.py 的 {question_id, question, scenario, evidence_memory_ids, interactions, expected_answer}

⚠️ 因真實 HF schema 未在此環境驗證，映射屬 PENDING-LIVE：實際 full.jsonl 產出需在有網路
   ＋有 `datasets` 的環境跑一次，並依印出的實際欄位微調 --field-map。

CLI：
  python -m duduclaw.memory_eval.fetch_benchmarks personamem
  python -m duduclaw.memory_eval.fetch_benchmarks longmemeval --hf-repo <repo_id>
  python -m duduclaw.memory_eval.fetch_benchmarks all

M1 記憶評測接軌
"""
from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path
from typing import Optional

from . import longmemeval_v2 as lme
from . import personamem_v2 as pm

logger = logging.getLogger(__name__)

# PersonaMem-v2 的 HF repo 由任務明確給定；LongMemEval-V2 未給定 repo id，
# 需操作者以 --hf-repo 提供（不捏造未驗證的 repo id）。
PERSONAMEM_HF_REPO = "bowen-upenn/PersonaMem-v2"


class FetchError(RuntimeError):
    """下載/轉檔失敗——一律帶清楚的下一步指令。"""


def _require_datasets():
    """惰性載入 HuggingFace datasets；缺套件則誠實 fail。"""
    try:
        import datasets  # noqa: F401
        return datasets
    except ImportError as e:
        raise FetchError(
            "缺少 `datasets` 套件，無法從 HuggingFace 下載。\n"
            "  安裝：pip install datasets\n"
            "  （若在受限環境，請改在有網路的機器下載後把 full.jsonl 複製過來。）"
        ) from e


def _first_field(row: dict, candidates: list[str]) -> Optional[object]:
    """從候選欄位名取第一個存在的值。"""
    for c in candidates:
        if c in row and row[c] not in (None, ""):
            return row[c]
    return None


def _dump_raw(rows: list[dict], raw_path: Path) -> None:
    """逐列原檔落地，確保下載成果不因映射失敗而遺失。"""
    raw_path.parent.mkdir(parents=True, exist_ok=True)
    with open(raw_path, "w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False, default=str) + "\n")
    logger.info("Raw dump written: %s (%d rows)", raw_path, len(rows))


def _load_hf_rows(repo_id: str, split: str, config_name: Optional[str]) -> list[dict]:
    """下載 HF dataset 並回傳 list[dict]；網路/憑證錯誤誠實 fail。"""
    datasets = _require_datasets()
    try:
        ds = datasets.load_dataset(repo_id, name=config_name, split=split)
    except Exception as e:  # network / auth / repo-not-found / split-not-found
        raise FetchError(
            f"無法從 HuggingFace 載入 `{repo_id}` (split={split}).\n"
            f"  原因：{type(e).__name__}: {e}\n"
            "  檢查：① 網路連線 ② 是否需要 `huggingface-cli login`（私有/gated 資料集）\n"
            "        ③ repo id / split 是否正確（可用 datasets.get_dataset_config_names 查）"
        ) from e
    return [dict(r) for r in ds]


def fetch_personamem(
    out_path: Path = pm.FULL_PATH,
    split: str = "test",
    hf_repo: str = PERSONAMEM_HF_REPO,
    config_name: Optional[str] = None,
) -> int:
    """下載 PersonaMem-v2 → 轉成 personamem_v2 loader 格式。回傳寫入題數。"""
    rows = _load_hf_rows(hf_repo, split, config_name)
    _dump_raw(rows, out_path.with_suffix(".raw.jsonl"))

    converted: list[dict] = []
    for i, row in enumerate(rows):
        question = _first_field(row, ["question", "query", "prompt", "instruction"])
        if question is None:
            continue
        qid = _first_field(row, ["question_id", "id", "qid"]) or f"pm-{i:05d}"
        scenario = _first_field(row, ["scenario", "topic", "category", "context_type"]) or "general"
        evidence = _first_field(row, ["evidence_memory_ids", "evidence_ids", "gold_ids"]) or []
        interactions = _first_field(
            row, ["interactions", "context", "sessions", "history", "haystack"]
        ) or []
        expected = _first_field(row, ["expected_answer", "answer", "label", "correct_answer"])
        converted.append({
            "question_id": str(qid),
            "question": str(question),
            "scenario": str(scenario),
            "evidence_memory_ids": evidence if isinstance(evidence, list) else [str(evidence)],
            "interactions": interactions if isinstance(interactions, list) else [],
            "expected_answer": None if expected is None else str(expected),
        })

    if not converted:
        raise FetchError(
            f"PersonaMem-v2 下載成功但欄位映射失敗（0 題轉出）。\n"
            f"  實際欄位：{sorted(rows[0].keys()) if rows else '(空)'}\n"
            f"  原檔已保留：{out_path.with_suffix('.raw.jsonl')}\n"
            "  請依實際欄位調整 fetch_personamem() 的候選欄位名再重跑。"
        )

    _write_jsonl(converted, out_path)
    logger.info("PersonaMem-v2 → %s (%d questions)", out_path, len(converted))
    return len(converted)


def fetch_longmemeval(
    out_path: Path = lme.FULL_PATH,
    split: str = "test",
    hf_repo: Optional[str] = None,
    config_name: Optional[str] = None,
) -> int:
    """下載 LongMemEval-V2 → 轉成 longmemeval_v2 loader 格式。回傳寫入題數。

    LongMemEval-V2（arXiv:2605.12493）的 HF repo id 未在本任務中給定/驗證，
    因此必須由操作者以 --hf-repo 提供，避免捏造未驗證的來源。
    """
    if not hf_repo:
        raise FetchError(
            "LongMemEval-V2 需以 --hf-repo 指定 HuggingFace repo id（本任務未提供已驗證來源）。\n"
            "  取得指引：\n"
            "    1. 讀論文 arXiv:2605.12493 的 'Data / Code Availability' 段找官方 repo。\n"
            "    2. 於 HuggingFace 搜 'LongMemEval' 確認正確 repo id 與 split。\n"
            "    3. 重跑：python -m duduclaw.memory_eval.fetch_benchmarks longmemeval --hf-repo <repo_id> --split <split>\n"
            "  （在確認正確來源前，離線驗證請用 data/longmemeval_v2/sample.jsonl。）"
        )

    rows = _load_hf_rows(hf_repo, split, config_name)
    _dump_raw(rows, out_path.with_suffix(".raw.jsonl"))

    converted: list[dict] = []
    for i, row in enumerate(rows):
        question = _first_field(row, ["question", "query", "prompt"])
        if question is None:
            continue
        qid = _first_field(row, ["question_id", "id", "qid"]) or f"lme-{i:05d}"
        ability = _first_field(
            row, ["ability", "question_type", "type", "task_type"]
        ) or "information_extraction"
        evidence = _first_field(
            row, ["evidence_memory_ids", "answer_session_ids", "evidence_ids", "gold_ids"]
        ) or []
        haystack = _first_field(
            row, ["haystack", "haystack_sessions", "context", "sessions", "memories"]
        ) or []
        answer = _first_field(row, ["answer", "expected_answer", "label"])
        converted.append({
            "question_id": str(qid),
            "question": str(question),
            "ability": str(ability),
            "evidence_memory_ids": evidence if isinstance(evidence, list) else [str(evidence)],
            "haystack": haystack if isinstance(haystack, list) else [],
            "answer": None if answer is None else str(answer),
        })

    if not converted:
        raise FetchError(
            f"LongMemEval-V2 下載成功但欄位映射失敗（0 題轉出）。\n"
            f"  實際欄位：{sorted(rows[0].keys()) if rows else '(空)'}\n"
            f"  原檔已保留：{out_path.with_suffix('.raw.jsonl')}\n"
            "  請依實際欄位調整 fetch_longmemeval() 的候選欄位名再重跑。"
        )

    _write_jsonl(converted, out_path)
    logger.info("LongMemEval-V2 → %s (%d questions)", out_path, len(converted))
    return len(converted)


def _write_jsonl(rows: list[dict], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="下載並轉檔 LongMemEval-V2 / PersonaMem-v2 記憶 benchmark",
    )
    parser.add_argument(
        "benchmark",
        choices=["longmemeval", "personamem", "all"],
        help="要下載的 benchmark",
    )
    parser.add_argument("--hf-repo", default=None, help="HuggingFace repo id（LongMemEval 必填）")
    parser.add_argument("--split", default="test", help="dataset split（預設 test）")
    parser.add_argument("--config-name", default=None, help="dataset config name（如有）")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args(argv)

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )

    rc = 0
    try:
        if args.benchmark in ("personamem", "all"):
            n = fetch_personamem(
                split=args.split,
                hf_repo=args.hf_repo or PERSONAMEM_HF_REPO,
                config_name=args.config_name,
            )
            print(f"✅ PersonaMem-v2: {n} questions → {pm.FULL_PATH}")
        if args.benchmark in ("longmemeval", "all"):
            n = fetch_longmemeval(
                split=args.split,
                hf_repo=args.hf_repo,
                config_name=args.config_name,
            )
            print(f"✅ LongMemEval-V2: {n} questions → {lme.FULL_PATH}")
    except FetchError as e:
        # 誠實 fail：印清楚指令，非零退出，不產生假資料
        print(f"\n🔴 PENDING-LIVE — 資料集取得失敗：\n{e}\n", file=sys.stderr)
        return 1

    return rc


if __name__ == "__main__":
    sys.exit(main())
