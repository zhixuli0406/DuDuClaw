# -*- coding: utf-8 -*-
from odoo import fields, models


class ResConfigSettings(models.TransientModel):
    _inherit = "res.config.settings"

    duduclaw_gateway_url = fields.Char(
        string="DuDuClaw Gateway URL",
        config_parameter="duduclaw.gateway_url",
        help="你的 DuDuClaw gateway HTTP API（duduclaw http-server），"
        "例如 http://192.168.1.10:8765。模組只會連線到這個位址。",
    )
    duduclaw_api_key = fields.Char(
        string="DuDuClaw API Key",
        config_parameter="duduclaw.api_key",
        help="gateway 的 MCP HTTP Bearer key（duduclaw mcp issue-refresh-token 簽發）。",
    )
    duduclaw_dashboard_url = fields.Char(
        string="DuDuClaw Dashboard URL",
        config_parameter="duduclaw.dashboard_url",
        help="儀表板位址（預設 gateway 的 18789 埠），選填，用於選單捷徑。",
    )

    def action_duduclaw_test_connection(self):
        self.ensure_one()
        connector = self.env["duduclaw.connector"]
        ok, message = connector.test_connection()
        notification_type = "success" if ok else "danger"
        return {
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "title": "DuDuClaw",
                "message": message,
                "type": notification_type,
                "sticky": not ok,
            },
        }
