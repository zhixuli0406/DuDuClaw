//! OS-native integration and appliance system operation.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "os_notify",
        description: "Show a native desktop notification on the host (macOS osascript / Linux notify-send). Requires the agent's [capabilities] os_native = true. Title/body are sanitized (injection-safe).",
        params: &[
            ParamDef {
                name: "title",
                description: "Notification title",
                required: true,
            },
            ParamDef {
                name: "body",
                description: "Notification body text",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "os_watch_status",
        description: "Report this agent's filesystem watcher status: watched paths and emitted / dropped event counts. Reads the gateway-maintained os_watch_stats.json. Requires [capabilities] os_native = true.",
        params: &[],
    },
    ToolDef {
        name: "os_open",
        description: "Open a file path or http(s) URL with the OS default handler. Only http/https and existing paths are allowed. Requires [capabilities] os_native; each call passes the VeriOS situation ASK gate.",
        params: &[ParamDef {
            name: "target",
            description: "An existing file path, or an http(s):// URL",
            required: true,
        }],
    },
    ToolDef {
        name: "os_frontmost",
        description: "Report the host's frontmost application and its focused window title (macOS System Events; Linux xdotool; Windows unsupported). Read-only. Requires [capabilities] os_native = true.",
        params: &[],
    },
    ToolDef {
        name: "os_spotlight_search",
        description: "Search the macOS Spotlight metadata index (`mdfind`) for files matching a query, optionally scoped to a directory. Read-only, macOS only. Requires the agent's [capabilities] os_native = true.",
        params: &[
            ParamDef {
                name: "query",
                description: "Search query text",
                required: true,
            },
            ParamDef {
                name: "scope_dir",
                description: "Optional directory to scope the search to (must exist)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max results to return (default 20, max 200)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "os_calendar_today",
        description: "Read today's calendar events (title / start / end / calendar name) from the host's Calendar.app via JXA, read-only, macOS only. Requires the agent's [capabilities] os_native = true.",
        params: &[],
    },
    ToolDef {
        name: "os_device_status",
        description: "Read the appliance's CPU/RAM/disk/temperature/uptime/network snapshot. admin, appliance-only. Bridges the dashboard-only device.status RPC to agents (O-0).",
        params: &[],
    },
    ToolDef {
        name: "os_system_status",
        description: "Read a reduced system status snapshot (version, agent count, appliance flag, edition profile, best-effort channel count). admin. Live-only fields are unavailable out-of-process and omitted.",
        params: &[],
    },
    ToolDef {
        name: "os_check_update",
        description: "Check for an available duduclaw self-update and (appliance only) an OS image update. admin. `device_check` is the real upstream freshness signal; prefer it over `device` for \"is there a new version\".",
        params: &[],
    },
    ToolDef {
        name: "os_backup_list",
        description: "List device backups stored under <home>/backups/. admin, appliance-only. Bridges the dashboard-only device.backup_list RPC to agents (O-0).",
        params: &[],
    },
    ToolDef {
        name: "os_network_info",
        description: "Read the appliance's network interfaces. admin, appliance-only. Bridges the dashboard-only device.network RPC to agents (O-0).",
        params: &[],
    },
    ToolDef {
        name: "os_wifi_status",
        description: "Read Wi-Fi link state, IP info and internet/captive-portal connectivity. admin, appliance-only. Richer than os_network_info, which is only a bare interface list.",
        params: &[],
    },
    ToolDef {
        name: "os_wifi_scan",
        description: "Scan for nearby Wi-Fi networks (SSID, signal bars, security type, known/connected flags). admin, appliance-only, read-only. Joining a secured network needs a human to enter the passphrase.",
        params: &[ParamDef {
            name: "rescan",
            description: "Trigger a fresh iwd scan before reading results. Defaults to true.",
            required: false,
        }],
    },
    ToolDef {
        name: "os_wifi_connect",
        description: "Join a Wi-Fi network by SSID. admin, appliance-only, destructive: requires confirm:true. Has no password parameter by design — it can only join an open or already-known network.",
        params: &[
            ParamDef {
                name: "ssid",
                description: "Network name to join",
                required: true,
            },
            ParamDef {
                name: "confirm",
                description: "Must be true — this changes the box's active network",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "os_apply_update",
        description: "Apply an update. admin, destructive, requires confirm:true. `target`: \"device\" (appliance OS image, full verify/stage/backup/install) or \"system\" (duduclaw self-update).",
        params: &[
            ParamDef {
                name: "target",
                description: "\"device\" or \"system\" — which update to apply",
                required: true,
            },
            ParamDef {
                name: "confirm",
                description: "Must be true — this is destructive",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "os_boot_assessment",
        description: "Read systemd's automatic boot assessment (good/bad/indeterminate/clean) for the running version. admin, appliance-only, read-only — how you check whether an update you applied actually took.",
        params: &[],
    },
    ToolDef {
        name: "os_update_rollback",
        description: "Roll back to the previously-installed A/B slot, then reboot. admin, appliance-only, destructive: requires confirm:true. Recoverable — same tier as os_power, not os_factory_reset.",
        params: &[ParamDef {
            name: "confirm",
            description: "Must be true — this is destructive",
            required: true,
        }],
    },
    ToolDef {
        name: "os_backup_create",
        description: "Archive the device's writable data partition for download. admin, appliance-only. Bridges the dashboard-only device.backup_create RPC to agents (O-0).",
        params: &[],
    },
    ToolDef {
        name: "os_power",
        description: "Restart or shut down the appliance. admin, appliance-only, destructive: requires confirm:true.",
        params: &[
            ParamDef {
                name: "action",
                description: "\"restart\" or \"shutdown\"",
                required: true,
            },
            ParamDef {
                name: "confirm",
                description: "Must be true — this is destructive",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "os_factory_reset",
        description: "Wipe device state and re-provision on next boot. admin, appliance-only, IRREVERSIBLE: requires confirm:true AND live human approval via ApprovalBroker (an agent's confirm is not a human decision).",
        params: &[ParamDef {
            name: "confirm",
            description: "Must be true — this is irreversible",
            required: true,
        }],
    },
    ToolDef {
        name: "os_doctor_repair",
        description: "Run a reduced set of health checks (config file / agents / API key / MCP cold-start) with repair hints. admin. Omits the dashboard's heavier container-runtime and grok-cli probes.",
        params: &[],
    },
    ToolDef {
        name: "os_display_get",
        description: "Read the appliance's current display appearance: cursor size/source (plus effective size, theme, persistence) and the primary screen's UI scale percentage. admin, appliance-only, read-only.",
        params: &[],
    },
    ToolDef {
        name: "os_display_set",
        description: "Change one display field live (cursor_size / cursor_source / theme / output_scale). admin, appliance-only, reversible. `value` is always a string; an out-of-set value is refused, never clamped.",
        params: &[
            ParamDef {
                name: "field",
                description: "cursor_size | cursor_source | theme | output_scale",
                required: true,
            },
            ParamDef {
                name: "value",
                description: "The new value, as a string (e.g. \"150\" for output_scale)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "os_audio_get",
        description: "Read the appliance's current audio state: volume percentage, mute, and every output device (id/name/is_default). admin, appliance-only, read-only.",
        params: &[],
    },
    ToolDef {
        name: "os_audio_set",
        description: "Change one audio field live: volume (0-100) / mute ('toggle' only) / output (a device id from os_audio_get). admin, appliance-only, reversible. `value` is always a string; out-of-range is refused.",
        params: &[
            ParamDef {
                name: "field",
                description: "volume | mute | output",
                required: true,
            },
            ParamDef {
                name: "value",
                description: "The new value, as a string (e.g. \"70\" for volume, \"toggle\" for mute)",
                required: true,
            },
        ],
    },
];
