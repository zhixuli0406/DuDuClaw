"""
memory_eval/client.py
MemoryClient 抽象介面，可對接 DuDuClaw MCP 或直接 DB 查詢

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import os
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Optional

from .config import EvalConfig


@dataclass
class Memory:
    memory_id:        str
    content:          str
    summary:          Optional[str]
    importance_score: float
    layer:            str              # 'episodic' | 'semantic' | 'procedural'
    entity_type:      Optional[str]  = None
    entity_id:        Optional[str]  = None
    attribute:        Optional[str]  = None
    valid_until:      Optional[str]  = None
    created_at:       Optional[str]  = None


@dataclass
class SearchResult:
    memory_id:  str
    content:    str
    similarity: float


class MemoryClient(ABC):
    """記憶系統存取介面（可替換底層實作）"""

    @abstractmethod
    async def search(
        self,
        query: str,
        limit: int = 5,
        namespace: Optional[str] = None,
    ) -> list[SearchResult]:
        """語意搜尋，回傳含 similarity 的結果列表"""

    @abstractmethod
    async def store(
        self,
        content: str,
        tags: Optional[list[str]] = None,
        namespace: Optional[str] = None,
    ) -> str:
        """儲存記憶，回傳 memory_id"""

    @abstractmethod
    async def list_important(
        self,
        agent_id: str,
        min_importance: float = 0.7,
        limit: int = 500,
    ) -> list[Memory]:
        """取得高重要性記憶列表"""

    @abstractmethod
    async def list_active(self, agent_id: str) -> list[Memory]:
        """取得所有 active 記憶（valid_until IS NULL）"""

    @abstractmethod
    async def get_by_ids(self, memory_ids: list[str]) -> list[Memory]:
        """依 ID 列表取得記憶"""

    @abstractmethod
    async def get_episodic_pressure(self, hours_ago: int = 24) -> float:
        """取得 episodic pressure 值（0~10+ 範圍，未正規化）"""


class HttpMemoryClient(MemoryClient):
    """
    透過 DuDuClaw HTTP/MCP API 存取記憶的具體實作。
    生產環境使用，需設定 DUDUCLAW_API_URL 與 DUDUCLAW_API_KEY 環境變數。
    """

    def __init__(self, base_url: str, api_key: str, agent_id: str) -> None:
        self._base_url = base_url.rstrip("/")
        self._api_key = api_key
        self._agent_id = agent_id

    async def search(
        self,
        query: str,
        limit: int = 5,
        namespace: Optional[str] = None,
    ) -> list[SearchResult]:
        import aiohttp
        headers = {"Authorization": f"Bearer {self._api_key}"}
        payload = {"query": query, "limit": limit}
        if namespace:
            payload["namespace"] = namespace

        async with aiohttp.ClientSession() as session:
            async with session.post(
                f"{self._base_url}/memory/search",
                json=payload,
                headers=headers,
            ) as resp:
                resp.raise_for_status()
                data = await resp.json()
                return [
                    SearchResult(
                        memory_id=m["memory_id"],
                        content=m["content"],
                        similarity=m.get("similarity", 0.0),
                    )
                    for m in data.get("memories", [])
                ]

    async def store(
        self,
        content: str,
        tags: Optional[list[str]] = None,
        namespace: Optional[str] = None,
    ) -> str:
        import aiohttp
        headers = {"Authorization": f"Bearer {self._api_key}"}
        payload: dict = {"content": content}
        if tags:
            payload["tags"] = tags
        if namespace:
            payload["namespace"] = namespace

        async with aiohttp.ClientSession() as session:
            async with session.post(
                f"{self._base_url}/memory/store",
                json=payload,
                headers=headers,
            ) as resp:
                resp.raise_for_status()
                data = await resp.json()
                return data["id"]

    async def list_important(
        self,
        agent_id: str,
        min_importance: float = 0.7,
        limit: int = 500,
    ) -> list[Memory]:
        raise NotImplementedError("list_important requires direct DB access")

    async def list_active(self, agent_id: str) -> list[Memory]:
        raise NotImplementedError("list_active requires direct DB access")

    async def get_by_ids(self, memory_ids: list[str]) -> list[Memory]:
        raise NotImplementedError("get_by_ids requires direct DB access")

    async def get_episodic_pressure(self, hours_ago: int = 24) -> float:
        import aiohttp
        headers = {"Authorization": f"Bearer {self._api_key}"}
        async with aiohttp.ClientSession() as session:
            async with session.get(
                f"{self._base_url}/memory/episodic-pressure",
                params={"hours_ago": hours_ago},
                headers=headers,
            ) as resp:
                resp.raise_for_status()
                data = await resp.json()
                return float(data["pressure"])


def build_client(config: EvalConfig) -> MemoryClient:
    """
    根據環境變數建立適當的 MemoryClient 實作。
    - 如有 DUDUCLAW_API_URL → 使用 HttpMemoryClient
    - 否則 raise RuntimeError（需明確設定環境）
    """
    api_url = os.environ.get("DUDUCLAW_API_URL", "")
    api_key = os.environ.get("DUDUCLAW_API_KEY", "")

    if not api_url:
        raise RuntimeError(
            "DUDUCLAW_API_URL is not set. "
            "Set it to the DuDuClaw API base URL (e.g. http://localhost:8080)."
        )
    if not api_key:
        raise RuntimeError(
            "DUDUCLAW_API_KEY is not set. "
            "Set it to a valid DuDuClaw API key."
        )

    return HttpMemoryClient(
        base_url=api_url,
        api_key=api_key,
        agent_id=config.agent_id,
    )
