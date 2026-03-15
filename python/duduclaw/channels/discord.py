import logging
from typing import List, Optional

from .base import ChannelPlugin

logger = logging.getLogger(__name__)


class DiscordChannel(ChannelPlugin):
    """Discord Bot channel plugin"""

    name = "discord"

    def __init__(
        self,
        bot_token: str,
        guild_ids: Optional[List[str]] = None,
    ):
        self.bot_token = bot_token
        self.guild_ids = guild_ids or []
        self._connected = False

    async def connect(self) -> None:
        """Start Discord bot client"""
        logger.info("Discord channel connecting...")
        self._connected = True
        # TODO: Use discord.py to start bot
        # self._client = discord.Client(intents=...)
        logger.info("Discord channel connected")

    async def send_message(self, chat_id: str, text: str) -> None:
        """Send message to Discord channel"""
        if not self._connected:
            raise RuntimeError("Discord channel not connected")
        logger.info(f"Discord -> {chat_id}: {text[:50]}...")
        # TODO: Implement actual Discord API call

    async def disconnect(self) -> None:
        """Stop Discord bot"""
        self._connected = False
        logger.info("Discord channel disconnected")

    def is_connected(self) -> bool:
        return self._connected

    def owns_chat_id(self, chat_id: str) -> bool:
        return chat_id.startswith("discord:")
