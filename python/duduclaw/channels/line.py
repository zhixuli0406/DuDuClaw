import logging
from typing import Optional

from .base import ChannelPlugin

logger = logging.getLogger(__name__)


class LineChannel(ChannelPlugin):
    """LINE Messaging API channel plugin"""

    name = "line"

    def __init__(
        self,
        channel_token: str,
        channel_secret: str,
        webhook_port: int = 8080,
    ):
        self.channel_token = channel_token
        self.channel_secret = channel_secret
        self.webhook_port = webhook_port
        self._connected = False
        self._webhook_server: Optional[object] = None

    async def connect(self) -> None:
        """Start LINE webhook server"""
        logger.info(f"LINE channel connecting on port {self.webhook_port}")
        self._connected = True
        # TODO: Start actual aiohttp webhook server for LINE
        logger.info("LINE channel connected")

    async def send_message(self, chat_id: str, text: str) -> None:
        """Send message via LINE Push API"""
        if not self._connected:
            raise RuntimeError("LINE channel not connected")
        logger.info(f"LINE -> {chat_id}: {text[:50]}...")
        # TODO: Implement actual LINE Push API call
        # POST https://api.line.me/v2/bot/message/push

    async def disconnect(self) -> None:
        """Stop LINE webhook server"""
        self._connected = False
        logger.info("LINE channel disconnected")

    def is_connected(self) -> bool:
        return self._connected

    def owns_chat_id(self, chat_id: str) -> bool:
        return chat_id.startswith("line:")
