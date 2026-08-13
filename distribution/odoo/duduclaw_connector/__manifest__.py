# -*- coding: utf-8 -*-
{
    "name": "DuDuClaw Connector",
    "summary": "Connect Odoo to your self-hosted DuDuClaw AI-employee platform",
    "description": """
Connect this Odoo to the DuDuClaw gateway you already run (self-hosted).

- Settings page: gateway URL + API key, with a connection test
- `duduclaw.connector` service model: send any text into your AI employee's
  memory from server actions / automations (e.g. file meeting notes, log
  customer context)
- Menu shortcut to open your DuDuClaw dashboard

External service disclosure: this module talks ONLY to the DuDuClaw gateway
URL configured in Settings — typically a server you own. No third-party
service, no telemetry. DuDuClaw itself: https://github.com/zhixuli0406/DuDuClaw
""",
    "version": "18.0.0.1.0",
    "category": "Productivity",
    "author": "DuDu Digital",
    "website": "https://github.com/zhixuli0406/DuDuClaw",
    "license": "LGPL-3",
    "depends": ["base", "base_setup"],
    "data": [
        "views/res_config_settings_views.xml",
    ],
    "installable": True,
    "application": False,
}
