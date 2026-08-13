# -*- coding: utf-8 -*-
import json
import logging
import urllib.error
import urllib.request

from odoo import api, models
from odoo.exceptions import UserError

_logger = logging.getLogger(__name__)

TIMEOUT_SECS = 15
MAX_TEXT_BYTES = 16 * 1024


class DuduclawConnector(models.AbstractModel):
    """Thin service layer to the customer's own DuDuClaw gateway.

    Deliberately stdlib-only (urllib): app-store friendly, zero extra
    Python deps on the Odoo host. Talks ONLY to the configured gateway URL.
    """

    _name = "duduclaw.connector"
    _description = "DuDuClaw gateway connector"

    @api.model
    def _config(self):
        icp = self.env["ir.config_parameter"].sudo()
        url = (icp.get_param("duduclaw.gateway_url") or "").strip().rstrip("/")
        key = (icp.get_param("duduclaw.api_key") or "").strip()
        return url, key

    @api.model
    def _post_json(self, path, payload):
        url, key = self._config()
        if not url:
            raise UserError("尚未設定 DuDuClaw Gateway URL（設定 → 一般設定 → DuDuClaw）")
        if not key:
            raise UserError("尚未設定 DuDuClaw API Key（設定 → 一般設定 → DuDuClaw）")
        if not url.startswith(("http://", "https://")):
            raise UserError("DuDuClaw Gateway URL 必須是 http(s):// 開頭")
        req = urllib.request.Request(
            f"{url}{path}",
            data=json.dumps(payload).encode("utf-8"),
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {key}",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=TIMEOUT_SECS) as resp:
                return json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8", errors="replace")[:200]
            _logger.warning("DuDuClaw gateway HTTP %s: %s", e.code, body)
            raise UserError(f"DuDuClaw gateway 回應 HTTP {e.code}：{body}") from e
        except urllib.error.URLError as e:
            raise UserError(f"連不上 DuDuClaw gateway：{e.reason}") from e

    @api.model
    def test_connection(self):
        """Settings-page connection test. Returns (ok, human message)."""
        url, key = self._config()
        if not url or not key:
            return False, "請先填 Gateway URL 與 API Key"
        try:
            req = urllib.request.Request(f"{url}/healthz", method="GET")
            with urllib.request.urlopen(req, timeout=TIMEOUT_SECS) as resp:
                if resp.status == 200:
                    return True, "連線成功 🐾（gateway 健康檢查通過）"
                return False, f"gateway 回應 HTTP {resp.status}"
        except Exception as e:  # noqa: BLE001 - settings-page UX, report anything
            return False, f"連不上 gateway：{e}"

    @api.model
    def send_to_memory(self, text, source="odoo"):
        """Store text into the AI employee's memory (server actions /
        automations call this). Routed through the gateway's transcript-ingest
        endpoint — scope checks and origin binding happen gateway-side."""
        text = (text or "").strip()
        if not text:
            raise UserError("沒有內容可以送")
        payload = {"source": source, "text": text[:MAX_TEXT_BYTES]}
        result = self._post_json("/ingest/transcript", payload)
        if not result.get("stored"):
            raise UserError(f"DuDuClaw 未接受這筆內容：{json.dumps(result)[:200]}")
        return True
