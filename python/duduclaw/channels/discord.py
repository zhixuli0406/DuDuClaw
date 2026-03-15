"""Discord Bot channel plugin"""
import asyncio
import logging
from typing import List, Optional

from .base import ChannelPlugin

logger = logging.getLogger(__name__)

DISCORD_API = "https://discord.com/api/v10"


class DiscordChannel(ChannelPlugin):
    """Discord Bot channel plugin using raw HTTP"""

    name = "discord"

    def __init__(
        self,
        bot_token: str,
        guild_ids: Optional[List[str]] = None,
    ):
        self.bot_token = bot_token
        self.guild_ids = guild_ids or []
        self._connected = False
        self._http: Optional[object] = None
        self._bot_info: Optional[dict] = None

    async def connect(self) -> None:
        """Verify bot token with Discord API"""
        try:
            import httpx
        except ImportError:
            raise RuntimeError(
                "httpx is required for Discord channel. Install with: pip install httpx"
            )

        self._http = httpx.AsyncClient(
            timeout=30.0,
            headers={"Authorization": f"Bot {self.bot_token}"},
        )

        try:
            resp = await self._http.get(f"{DISCORD_API}/users/@me")
            if resp.status_code == 200:
                self._bot_info = resp.json()
                bot_name = self._bot_info.get("username", "unknown")
                logger.info("Discord bot connected: %s", bot_name)
            elif resp.status_code == 401:
                raise RuntimeError("Discord: Invalid bot token (401 Unauthorized)")
            else:
                raise RuntimeError(
                    "Discord: Unexpected status %d: %s" % (resp.status_code, resp.text[:200])
                )
        except RuntimeError:
            raise
        except Exception as e:
            raise RuntimeError("Discord connection failed: %s" % e) from e

        self._connected = True

    async def send_message(self, chat_id: str, text: str) -> None:
        """Send message to a Discord channel"""
        if not self._connected or self._http is None:
            raise RuntimeError("Discord channel not connected")

        channel_id = chat_id.removeprefix("discord:")

        try:
            resp = await self._http.post(
                f"{DISCORD_API}/channels/{channel_id}/messages",
                json={"content": text},
            )
            if resp.status_code in (200, 201):
                logger.info("Discord -> %s: %s", chat_id, text[:50])
            else:
                logger.error(
                    "Discord send failed (%d): %s",
                    resp.status_code,
                    resp.text[:200],
                )
        except Exception as e:
            logger.error("Discord sendMessage error: %s", e)

    async def disconnect(self) -> None:
        """Close HTTP client"""
        self._connected = False
        if self._http is not None:
            await self._http.aclose()
            self._http = None
        logger.info("Discord channel disconnected")

    def is_connected(self) -> bool:
        return self._connected

    def owns_chat_id(self, chat_id: str) -> bool:
        return chat_id.startswith("discord:")
