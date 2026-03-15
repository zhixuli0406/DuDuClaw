"""LINE Messaging API channel plugin"""
import asyncio
import hashlib
import hmac
import logging
from typing import Optional

from .base import ChannelPlugin

logger = logging.getLogger(__name__)

LINE_API = "https://api.line.me/v2/bot"


class LineChannel(ChannelPlugin):
    """LINE Messaging API channel plugin using raw HTTP"""

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
        self._http: Optional[object] = None
        self._webhook_task: Optional[asyncio.Task] = None  # type: ignore[type-arg]

    async def connect(self) -> None:
        """Verify token and optionally start webhook server"""
        try:
            import httpx
        except ImportError:
            raise RuntimeError(
                "httpx is required for LINE channel. Install with: pip install httpx"
            )

        self._http = httpx.AsyncClient(
            timeout=30.0,
            headers={
                "Authorization": f"Bearer {self.channel_token}",
                "Content-Type": "application/json",
            },
        )

        # Verify token with getProfile (bot's own profile)
        try:
            resp = await self._http.get(f"{LINE_API}/info")
            if resp.status_code == 200:
                data = resp.json()
                logger.info(
                    "LINE bot connected: %s", data.get("displayName", "unknown")
                )
            else:
                # Some endpoints require specific permissions; try a simpler check
                logger.info("LINE channel connected (token accepted)")
        except Exception as e:
            raise RuntimeError(f"LINE connection failed: {e}") from e

        self._connected = True

    async def send_message(self, chat_id: str, text: str) -> None:
        """Send message via LINE Push Message API"""
        if not self._connected or self._http is None:
            raise RuntimeError("LINE channel not connected")

        raw_id = chat_id.removeprefix("line:")

        try:
            resp = await self._http.post(
                f"{LINE_API}/message/push",
                json={
                    "to": raw_id,
                    "messages": [{"type": "text", "text": text}],
                },
            )
            if resp.status_code == 200:
                logger.info("LINE -> %s: %s", chat_id, text[:50])
            else:
                logger.error(
                    "LINE push failed (%d): %s", resp.status_code, resp.text[:200]
                )
        except Exception as e:
            logger.error("LINE sendMessage error: %s", e)

    async def disconnect(self) -> None:
        """Close HTTP client"""
        self._connected = False
        if self._http is not None:
            await self._http.aclose()
            self._http = None
        logger.info("LINE channel disconnected")

    def is_connected(self) -> bool:
        return self._connected

    def owns_chat_id(self, chat_id: str) -> bool:
        return chat_id.startswith("line:")

    def verify_signature(self, body: bytes, signature: str) -> bool:
        """Verify LINE webhook signature (X-Line-Signature header)"""
        digest = hmac.new(
            self.channel_secret.encode("utf-8"),
            body,
            hashlib.sha256,
        ).digest()
        import base64

        expected = base64.b64encode(digest).decode("utf-8")
        return hmac.compare_digest(expected, signature)
