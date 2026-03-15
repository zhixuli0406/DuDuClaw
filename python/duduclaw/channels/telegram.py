import logging
from typing import Optional

from .base import ChannelPlugin

logger = logging.getLogger(__name__)


class TelegramChannel(ChannelPlugin):
    """Telegram Bot API channel plugin"""

    name = "telegram"

    def __init__(self, bot_token: str):
        self.bot_token = bot_token
        self._connected = False
        self._bot_info: Optional[dict] = None

    async def connect(self) -> None:
        """Start Telegram polling or webhook"""
        logger.info("Telegram channel connecting...")
        self._connected = True
        # TODO: Use python-telegram-bot to start polling
        # self._app = Application.builder().token(self.bot_token).build()
        logger.info("Telegram channel connected")

    async def send_message(self, chat_id: str, text: str) -> None:
        """Send message via Telegram Bot API"""
        if not self._connected:
            raise RuntimeError("Telegram channel not connected")
        logger.info(f"Telegram -> {chat_id}: {text[:50]}...")
        # TODO: Implement actual Telegram sendMessage API call

    async def disconnect(self) -> None:
        """Stop Telegram polling"""
        self._connected = False
        logger.info("Telegram channel disconnected")

    def is_connected(self) -> bool:
        return self._connected

    def owns_chat_id(self, chat_id: str) -> bool:
        return chat_id.startswith("tg:")
