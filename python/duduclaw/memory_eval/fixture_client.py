"""
memory_eval/fixture_client.py
InMemoryMemoryClient — 離線用的最小記憶客戶端

用途：讓 sample fixture 的 smoke test 能在無 DB / 無網路的環境下
真正跑一次 store → search → score 迴圈（不是靜態 mock map）。

搜尋採「CJK-safe 關鍵字重疊」樸素評分，刻意不模擬向量檢索——
它只是離線驗證管線接線是否正確的替身，不是真實 SqliteMemoryEngine。
真實檢索品質必須跑真實 dataset 對真實引擎（見 fetch_benchmarks.py）。

M1 記憶評測接軌 — LongMemEval-V2 / PersonaMem-v2
"""
from __future__ import annotations

import re
from typing import Optional

from .client import Memory, MemoryClient, SearchResult

# 中日韓統一表意文字 + 假名區段，用來做 CJK-safe 分詞
_CJK = r"一-鿿぀-ヿ가-힯"
_TOKEN_RE = re.compile(rf"[a-zA-Z0-9]+|[{_CJK}]")


def _tokenize(text: str) -> list[str]:
    """CJK-safe 樸素分詞：ASCII 詞整段、CJK 逐字。"""
    return [t.lower() for t in _TOKEN_RE.findall(text or "")]


def _overlap_score(query: str, content: str) -> float:
    """query 與 content 的 token 重疊比例（0~1），供樸素檢索排序。"""
    q_tokens = _tokenize(query)
    if not q_tokens:
        return 0.0
    c_tokens = set(_tokenize(content))
    hits = sum(1 for t in q_tokens if t in c_tokens)
    return hits / len(q_tokens)


class InMemoryMemoryClient(MemoryClient):
    """全記憶體、零依賴的 MemoryClient 實作，供離線 smoke / 單測使用。

    - store():   寫入記憶體 dict，回傳呼叫端指定或自動生成的 id
    - search():  對同 namespace 的記憶做 token 重疊評分，回傳 top-limit
    - get_by_ids(): 依 id 批次取回（對齊引擎 batch query API，上限 100）
    """

    def __init__(self) -> None:
        # memory_id -> (Memory, namespace)
        self._store: dict[str, tuple[Memory, Optional[str]]] = {}
        self._auto_seq = 0

    def preload(
        self,
        memory_id: str,
        content: str,
        namespace: Optional[str] = None,
    ) -> None:
        """直接以指定 id 預載一筆記憶（給 fixture haystack 用）。"""
        self._store[memory_id] = (
            Memory(
                memory_id=memory_id,
                content=content,
                summary=None,
                importance_score=0.5,
                layer="episodic",
            ),
            namespace,
        )

    async def search(
        self,
        query: str,
        limit: int = 5,
        namespace: Optional[str] = None,
    ) -> list[SearchResult]:
        scored: list[SearchResult] = []
        for mem, ns in self._store.values():
            if namespace is not None and ns != namespace:
                continue
            score = _overlap_score(query, mem.content)
            if score > 0:
                scored.append(
                    SearchResult(
                        memory_id=mem.memory_id,
                        content=mem.content,
                        similarity=score,
                    )
                )
        scored.sort(key=lambda r: r.similarity, reverse=True)
        return scored[:limit]

    async def store(
        self,
        content: str,
        tags: Optional[list[str]] = None,
        namespace: Optional[str] = None,
    ) -> str:
        self._auto_seq += 1
        memory_id = f"inmem-{self._auto_seq}"
        self.preload(memory_id, content, namespace)
        return memory_id

    async def list_important(
        self,
        agent_id: str,
        min_importance: float = 0.7,
        limit: int = 500,
    ) -> list[Memory]:
        return [
            mem
            for mem, _ in self._store.values()
            if mem.importance_score >= min_importance
        ][:limit]

    async def list_active(self, agent_id: str) -> list[Memory]:
        return [mem for mem, _ in self._store.values() if mem.valid_until is None]

    async def get_by_ids(self, memory_ids: list[str]) -> list[Memory]:
        if len(memory_ids) > 100:
            raise ValueError("batch fetch limited to 100 ids per request")
        out: list[Memory] = []
        for mid in memory_ids:
            entry = self._store.get(mid)
            if entry is not None:
                out.append(entry[0])
        return out

    async def get_episodic_pressure(self, hours_ago: int = 24) -> float:
        return float(len(self._store))
