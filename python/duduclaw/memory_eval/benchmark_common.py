"""
memory_eval/benchmark_common.py
共用工具：haystack 注入 + recall@k 評分

LongMemEval-V2 與 PersonaMem-v2 兩個 benchmark 都採同一套「檢索層級」評測法：
把每題的 context（haystack / 對話史）當記憶注入 SqliteMemoryEngine，
再對問題跑 search()，量 gold evidence 是否落在 top-K（recall@k）。

⚠️ 這是「記憶系統檢索能力」的 proxy 指標，非完整 QA answer-correctness。
   後者需 LLM judge（見各模組 docstring 的 PENDING-LIVE 說明）。

M1 記憶評測接軌
"""
from __future__ import annotations

import logging
from typing import Iterable, Optional

from .client import MemoryClient

logger = logging.getLogger(__name__)


async def ingest_haystack(
    client: MemoryClient,
    haystack: Iterable[dict],
    namespace: Optional[str] = None,
) -> dict[str, str]:
    """把一題的 haystack 記憶注入 client。

    haystack item 形如 {"memory_id": "...", "content": "..."}。
    - 若 client 是 InMemoryMemoryClient（有 preload），以原 id 預載，
      這樣 evidence_memory_ids 才對得上（離線 smoke 路徑）。
    - 否則走一般 store()，回傳 {原 id: 實際寫入 id} 映射
      （真實引擎路徑，供評分時 id 對照）。

    Returns:
        原 evidence id → 實際 memory id 的映射。
    """
    id_map: dict[str, str] = {}
    preload = getattr(client, "preload", None)
    for item in haystack:
        orig_id = str(item.get("memory_id", ""))
        content = str(item.get("content", ""))
        if not content:
            continue
        if callable(preload):
            preload(orig_id, content, namespace)
            id_map[orig_id] = orig_id
        else:
            new_id = await client.store(content=content, namespace=namespace)
            id_map[orig_id] = new_id
    return id_map


def recall_at_k(top_k_ids: list[str], evidence_ids: Iterable[str]) -> float:
    """gold evidence 有幾成落在 top-K。

    recall@k = |retrieved ∩ evidence| / |evidence|
    無 evidence → 回傳 0.0 並由呼叫端決定是否跳過。
    """
    evidence_set = {e for e in evidence_ids if e}
    if not evidence_set:
        return 0.0
    retrieved = set(top_k_ids)
    hit = len(evidence_set & retrieved)
    return hit / len(evidence_set)
