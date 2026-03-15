"""Telegram Bot API channel plugin"""
import asyncio
import logging
from typing import Optional

from .base import ChannelPlugin

logger = logging.getLogger(__name__)

TELEGRAM_API = "https://api.telegram.org"


class TelegramChannel(ChannelPlugin):
    """Telegram Bot API channel plugin using raw HTTP (no SDK dependency)"""

    name = "telegram"

    def __init__(self, bot_token: str):
        self.bot_token = bot_token
        self._connected = False
        self._bot_info: Optional[dict] = None
        self._http: Optional[object] = None  # httpx.AsyncClient
        self._polling_task: Optional[asyncio.Task] = None  # type: ignore[type-arg]

    @property
    def _api_base(self) -> str:
        return f"{TELEGRAM_API}/bot{self.bot_token}"

    async def connect(self) -> None:
        """Verify bot token and start polling"""
        try:
            import httpx
        except ImportError:
            raise RuntimeError(
                "httpx is required for Telegram channel. Install with: pip install httpx"
            )

        self._http = httpx.AsyncClient(timeout=30.0)

        # Verify token with getMe
        try:
            resp = await self._http.get(f"{self._api_base}/getMe")
            data = resp.json()
            if not data.get("ok"):
                desc = data.get("description", "Unknown error")
                raise RuntimeError(f"Telegram getMe failed: {desc}")
            self._bot_info = data["result"]
            bot_name = self._bot_info.get("username", "unknown")
            logger.info("Telegram bot connected: @%s", bot_name)
        except httpx.HTTPError as e:
            raise RuntimeError(f"Telegram connection failed: {e}") from e

        self._connected = True

        # Start long-polling in background
        self._polling_task = asyncio.create_task(self._poll_updates())

    async def send_message(self, chat_id: str, text: str) -> None:
        """Send message via Telegram sendMessage API"""
        if not self._connected or self._http is None:
            raise RuntimeError("Telegram channel not connected")

        # Strip tg: prefix if present
        raw_chat_id = chat_id.removeprefix("tg:")

        try:
            resp = await self._http.post(
                f"{self._api_base}/sendMessage",
                json={"chat_id": raw_chat_id, "text": text, "parse_mode": "Markdown"},
            )
            data = resp.json()
            if not data.get("ok"):
                desc = data.get("description", "Unknown error")
                logger.error("Telegram sendMessage failed: %s", desc)
            else:
                logger.info("Telegram -> %s: %s", chat_id, text[:50])
        except Exception as e:
            logger.error("Telegram sendMessage error: %s", e)

    async def disconnect(self) -> None:
        """Stop polling and close HTTP client"""
        self._connected = False
        if self._polling_task and not self._polling_task.done():
            self._polling_task.cancel()
            try:
                await self._polling_task
            except asyncio.CancelledError:
                pass
        if self._http is not None:
            await self._http.aclose()
            self._http = None
        logger.info("Telegram channel disconnected")

    def is_connected(self) -> bool:
        return self._connected

    def owns_chat_id(self, chat_id: str) -> bool:
        return chat_id.startswith("tg:")

    async def _poll_updates(self) -> None:
        """Long-poll Telegram getUpdates API"""
        offset = 0
        logger.info("Telegram polling started")
        while self._connected and self._http is not None:
            try:
                resp = await self._http.get(
                    f"{self._api_base}/getUpdates",
                    params={"offset": offset, "timeout": 25},
                    timeout=30.0,
                )
                data = resp.json()
                if data.get("ok") and data.get("result"):
                    for update in data["result"]:
                        offset = update["update_id"] + 1
                        self._handle_update(update)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.warning("Telegram polling error: %s", e)
                await asyncio.sleep(3)

        logger.info("Telegram polling stopped")

    def _handle_update(self, update: dict) -> None:
        """Process a single Telegram update"""
        message = update.get("message")
        if not message:
            return

        chat = message.get("chat", {})
        chat_id = str(chat.get("id", ""))
        sender = message.get("from", {}).get("username", "unknown")
        text = message.get("text", "")

        if text:
            logger.info("Telegram <- %s (@%s): %s", chat_id, sender, text[:80])
            self.on_message_received(f"tg:{chat_id}", sender, text)
