//! O-4: system-operator agent persona + guardrails — wires O-1's
//! `os_intent::route_os_intent` classifier into the conversational reply
//! path, but ONLY for an agent explicitly opted in via `agent.toml
//! [capabilities] system_operator = true` (the same capability O-0's
//! dispatch gate now requires — see `duduclaw-cli/src/mcp_dispatch.rs`'s
//! `SYSTEM_OPERATOR_TOOLS` check).
//!
//! ## Guardrails (maker-checker, mirrors O-0/O-1)
//!
//! - **This module never calls an `os_*` MCP tool handler.** It only
//!   decides how to shape ONE conversational turn — [`decide`] maps an
//!   [`OsIntentResult`] to an [`OperatorAction`], nothing more. Every O-0
//!   tool's own gate chain (Admin scope, the `system_operator` capability,
//!   `is_appliance()`, `confirm:true`, `ApprovalBroker` for
//!   `os_factory_reset`) is untouched and cannot be bypassed from here —
//!   this is a new front door, never a looser one.
//! - **Destructive intents are never auto-executed.** `needs_confirm` /
//!   `needs_approval` short-circuit the turn with a structured pending
//!   reply instead of letting the model attempt the tool call under
//!   pressure; the agent gets no chance to self-authorize.
//! - **Never a shell.** [`OsTool`] is the same closed enum O-1 emits — this
//!   module cannot manufacture a free-form command, only guide the model
//!   toward one of the ten named tools.
//! - **`Rejected` is never silently downgraded to `Chat`.** The router's own
//!   `reject_reason` is returned verbatim as the turn's reply.
//! - **`Chat` / `GoalTask` fall through completely unchanged** — this module
//!   never competes with the existing goal-intent path or ordinary
//!   conversation; [`decide`] returns [`OperatorAction::Continue`] and the
//!   caller proceeds exactly as if this module did not exist.
//! - **Fail-open for every other agent.** The caller only invokes this
//!   module at all when the resolved agent's `[capabilities]
//!   system_operator` is `true`; every other agent's reply-path behavior is
//!   byte-identical to before this module existed.
//! - **Every system-operation signal is audited.** [`audit_operator_decision`]
//!   appends one `security_audit.jsonl` row for every `SystemOp` or
//!   `Rejected` verdict (never for ordinary `Chat`/`GoalTask` turns, to
//!   avoid drowning the log) — "誰用一句話做了什麼" stays reconstructable
//!   independent of the O-0 tool's own `tool_calls.jsonl` record.
//!
//! ## Persona seed
//!
//! A dedicated free "system-operator" preset now ships in the public
//! `templates/presets/system-operator/` tree (installed via `duduclaw preset
//! install-builtin`, then `duduclaw agent create <name> --preset
//! system-operator`) — it materializes exactly the snippet below into the
//! agent's `agent.toml`. `apply_capabilities_to_table` also whitelists
//! `system_operator`, so the dashboard capability toggle sets it too. Either
//! way the effective `agent.toml [capabilities]` is:
//!
//! ```toml
//! [capabilities]
//! system_operator = true
//! # A dedicated operator persona should be trusted to act, not just chat —
//! # "operator" is the goal-loop autonomy level that still requires kickoff
//! # approval for anything beyond the O-0 tools' own gates.
//! autonomy_level = "operator"
//! ```
//!
//! The appliance's conversational front door (O-2's `/console`) should point
//! at whichever agent carries this flag; that wiring is O-2/O-3's, not this
//! module's.
//!
//! ## Task C: Guide-path RESULT cards
//!
//! Everything above only ever maps a PENDING confirmation (a destructive op
//! this module blocked from auto-running) to a chat artifact. Task C adds
//! the other half: when a `system_operator` agent's turn actually calls one
//! of the four read-only `os_*` tools (`os_device_status`/`os_check_update`/
//! `os_backup_list`/`os_network_info`) and the tool returns a structured
//! result, [`extract_readonly_result_artifact`] maps that result to the same
//! O-3 chat-artifact wire shape so the reply carries a live status card
//! instead of only prose. This module still never calls an MCP tool handler
//! — it only reads back evidence the CLI's own stream already captured (see
//! `channel_reply.rs`'s `spawn_claude_cli_with_env` for where that capture
//! happens, gated on the same `system_operator` capability).

use serde_json::{json, Value};

use crate::os_intent::{OsIntentCategory, OsIntentResult, OsTool};
use crate::runtime::NativeToolEvent;

// ═══════════════════════════════════════════════════════════════════════
// Decision — pure, total, fully unit-testable
// ═══════════════════════════════════════════════════════════════════════

/// What the reply-path caller should do with this turn, decided from a
/// single [`OsIntentResult`].
#[derive(Debug, Clone, PartialEq)]
pub enum OperatorAction {
    /// Short-circuit this turn with this exact reply text — the turn never
    /// reaches the LLM at all (zero cost, and structurally cannot invoke a
    /// tool).
    ShortCircuit(String),
    /// Continue the normal LLM pipeline, but append `hint` to the dynamic
    /// (never-cached) tail of the system prompt so the model is directed at
    /// the resolved tool + params. The actual tool call — if the model makes
    /// one — still passes through every existing MCP gate unchanged.
    Guide { tool: OsTool, hint: String },
    /// No actionable system-operator signal this turn — proceed exactly as
    /// if this module were not wired in.
    Continue,
}

/// Decide the [`OperatorAction`] for one [`OsIntentResult`]. Pure and total:
/// no I/O, never panics, always returns a value.
pub fn decide(result: &OsIntentResult) -> OperatorAction {
    match result.category {
        // Never compete with the goal-intent path or ordinary conversation.
        OsIntentCategory::Chat | OsIntentCategory::GoalTask => OperatorAction::Continue,
        // Refused outright — never silently downgraded to Chat.
        OsIntentCategory::Rejected => OperatorAction::ShortCircuit(render_rejected(result)),
        OsIntentCategory::SystemOp => {
            let Some(tool) = result.tool else {
                // Structurally unreachable — `OsIntentResult::system_op`
                // always sets `tool` when `category == SystemOp` — but this
                // module never trusts that invariant blindly. Fail-closed to
                // Continue (never guess a tool) rather than panic.
                return OperatorAction::Continue;
            };
            if !result.missing_params.is_empty() {
                return OperatorAction::ShortCircuit(render_clarify(result));
            }
            if result.needs_confirm || result.needs_approval {
                return OperatorAction::ShortCircuit(render_pending(tool, result));
            }
            OperatorAction::Guide { tool, hint: render_guide_hint(tool, result) }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Reply rendering (zh-TW, user-facing)
// ═══════════════════════════════════════════════════════════════════════

fn render_clarify(result: &OsIntentResult) -> String {
    result
        .clarify_prompt
        .clone()
        .unwrap_or_else(|| "請提供更多細節，我才能執行這個系統操作。".to_string())
}

fn render_rejected(result: &OsIntentResult) -> String {
    result
        .reject_reason
        .clone()
        .unwrap_or_else(|| "這個請求超出我能執行的系統操作範圍，已拒絕。".to_string())
}

fn tool_display_name(tool: OsTool) -> &'static str {
    match tool {
        OsTool::DeviceStatus => "查詢裝置狀態",
        OsTool::NetworkInfo => "查詢網路資訊",
        OsTool::WifiStatus => "查詢 Wi-Fi 連線狀態",
        OsTool::WifiScan => "掃描附近 Wi-Fi 網路",
        OsTool::WifiConnect => "連上 Wi-Fi 網路",
        OsTool::BackupList => "查詢備份清單",
        OsTool::SystemStatus => "查詢系統狀態",
        OsTool::CheckUpdate => "檢查更新",
        OsTool::BackupCreate => "建立備份",
        OsTool::ApplyUpdate => "套用更新",
        OsTool::BootAssessment => "查詢開機評估狀態",
        OsTool::UpdateRollback => "回退到上一個系統版本",
        OsTool::Power => "電源操作（重開機／關機）",
        OsTool::FactoryReset => "回復原廠設定",
        OsTool::DoctorRepair => "系統診斷",
    }
}

/// Opening/closing tag for the structured marker appended to a pending
/// reply. Mirrors `goal_intent.rs`'s `<goal_suggest>...</goal_suggest>`
/// convention (a plain literal tag, not a full templating scheme) so a
/// future inline-action-card renderer (O-3, tracked separately) can locate
/// and parse it with [`strip_system_operator_pending_tag`] without this
/// module needing to know anything about how O-3 renders a card.
const PENDING_TAG_OPEN: &str = "<system_operator_pending>";
const PENDING_TAG_CLOSE: &str = "</system_operator_pending>";

/// Render the human-readable half of a pending (destructive) reply, plus a
/// trailing structured JSON marker carrying `tool`/`params`/`needs_confirm`/
/// `needs_approval` — the O-0 tool is NEVER invoked here; this is text only.
fn render_pending(tool: OsTool, result: &OsIntentResult) -> String {
    let human = if result.needs_approval {
        format!(
            "「{}」是不可逆的系統操作，需要人工核准才能執行。已送出審批請求，請透過主控台核准或拒絕；\
             在核准之前我不會執行這個操作。",
            tool_display_name(tool)
        )
    } else {
        format!(
            "「{}」會變更這台機器的狀態，請先確認：要執行嗎？回覆「確認」即可繼續，或忽略此訊息取消。",
            tool_display_name(tool)
        )
    };
    let marker = json!({
        "tool": tool.as_str(),
        "params": result.params,
        "needs_confirm": result.needs_confirm,
        "needs_approval": result.needs_approval,
    });
    format!("{human}\n\n{PENDING_TAG_OPEN}{marker}{PENDING_TAG_CLOSE}")
}

/// Parse a reply produced by [`render_pending`] back into its human text and
/// structured marker payload — the O-3 counterpart to `goal_intent.rs`'s
/// `strip_goal_suggest_tag`. Fail-open on an unterminated/malformed tag: the
/// reply is returned unchanged with `None`, exactly like its `goal_intent`
/// sibling (never mangles a normal reply that happens to contain the literal
/// opening token, e.g. a quoted user paste).
pub fn strip_system_operator_pending_tag(reply: &str) -> (String, Option<Value>) {
    let Some(start) = reply.find(PENDING_TAG_OPEN) else {
        return (reply.to_string(), None);
    };
    let content_start = start + PENDING_TAG_OPEN.len();
    let Some(close_rel) = reply[content_start..].find(PENDING_TAG_CLOSE) else {
        return (reply.to_string(), None);
    };
    let content_end = content_start + close_rel;
    let tag_end = content_end + PENDING_TAG_CLOSE.len();

    let mut stripped = String::with_capacity(reply.len());
    stripped.push_str(reply[..start].trim_end());
    stripped.push(' ');
    stripped.push_str(reply[tag_end..].trim_start());
    let stripped = stripped.trim().to_string();

    match serde_json::from_str::<Value>(reply[content_start..content_end].trim()) {
        Ok(v) => (stripped, Some(v)),
        Err(_) => (reply.to_string(), None),
    }
}

/// O-4→O-3 wiring: map a [`strip_system_operator_pending_tag`] marker into
/// the O-3 chat-artifact wire shape (`{"type", "payload"}` — see
/// `web/src/components/console/artifact-types.ts`'s `ConfirmActionArtifact`).
/// This reuses O-3's SHAPE verbatim rather than inventing a new one (the same
/// discipline `artifact-types.ts`'s own doc comment asks of a future O-4
/// wiring pass); it does not import or reference the frontend module — the
/// two sides just agree on the JSON by convention, the same way every other
/// gateway↔web wire contract in this codebase does.
///
/// `os_power` (params.action ∈ {restart, shutdown}) and `os_factory_reset`
/// map to the `confirm_action` card; `os_apply_update`
/// (params.target ∈ {device, system}) maps to the `update_confirm` card
/// (`web/src/components/console/UpdateConfirmCard.tsx`) — the O-4→O-3
/// wiring this doc comment used to call a tracked gap. Every other tool, and
/// any marker missing/mistyping the fields this reads, returns `None` —
/// fail-closed to "no card" rather than guessing. Pure, total, never panics.
pub fn marker_to_artifact(marker: &Value) -> Option<Value> {
    let tool = marker.get("tool")?.as_str()?;
    match tool {
        "os_power" => {
            let action = marker.get("params")?.get("action")?.as_str()?;
            match action {
                "restart" | "shutdown" => {
                    Some(json!({ "type": "confirm_action", "payload": { "action": action } }))
                }
                _ => None,
            }
        }
        "os_factory_reset" => Some(json!({
            "type": "confirm_action",
            "payload": { "action": "factory_reset" }
        })),
        "os_apply_update" => {
            let target = marker.get("params")?.get("target")?.as_str()?;
            match target {
                "device" | "system" => {
                    Some(json!({ "type": "update_confirm", "payload": { "target": target } }))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Task C: Guide-path RESULT capture — result cards for a system-operator
// agent that actually called a read-only `os_*` tool this turn (as opposed
// to `marker_to_artifact` above, which only maps a PENDING confirmation
// marker rendered by THIS module before the LLM ever ran). Pure and total:
// every function here only reads already-captured, already-masked evidence
// (`crate::runtime::NativeToolEvent`, see that type's own masking contract)
// and never calls an MCP tool handler or does any I/O itself — same
// guardrail as the rest of this module.
// ═══════════════════════════════════════════════════════════════════════

/// Closed allowlist of read-only `os_*` tools this module renders a result
/// card for. Deliberately excludes every write/destructive `os_*` tool
/// (`os_power`, `os_apply_update`, `os_factory_reset`, `os_backup_create`,
/// `os_doctor_repair`) — those either go through the `ShortCircuit` pending
/// path above (never auto-executed) or have no O-3 result-card shape at all;
/// a tool outside this list is never turned into an artifact by this path,
/// full stop.
const READONLY_RESULT_TOOLS: &[&str] =
    &["os_device_status", "os_check_update", "os_backup_list", "os_network_info"];

/// Match a Claude-CLI-reported tool name against [`READONLY_RESULT_TOOLS`].
/// An MCP-served tool is reported qualified (`mcp__duduclaw__os_device_status`);
/// this matches on the final `__`-delimited segment, exact equality only —
/// never substring (project convention 2): `os_device_status_extra` must not
/// match `os_device_status`, and a bare (unqualified) name still matches.
fn readonly_result_tool_name(reported_name: &str) -> Option<&'static str> {
    let bare = reported_name.rsplit("__").next().unwrap_or(reported_name);
    READONLY_RESULT_TOOLS.iter().copied().find(|t| *t == bare)
}

/// True when `system` (the `os_check_update` self-version-check half) has the
/// `SystemUpdateCheckInfo` shape (`available` bool + `current_version` /
/// `latest_version` strings — mirrors the frontend's `SystemUpdateCheckInfo`
/// in `web/src/components/console/artifact-types.ts`) rather than the
/// `{"error": "..."}` failure shape `updater::check_update()` produces on
/// failure (`mcp_os_ops.rs`'s `handle_os_check_update`). Used to gate the
/// system-only `update_status` card in [`readonly_result_to_artifact`]: an
/// error-shaped or structurally incomplete `system` half can't stand alone as
/// a card, so it must never be treated as "valid" on its own.
fn system_half_is_valid(system: &Value) -> bool {
    system.get("available").and_then(Value::as_bool).is_some()
        && system.get("current_version").and_then(Value::as_str).is_some()
        && system.get("latest_version").and_then(Value::as_str).is_some()
}

/// Map one read-only `os_*` tool's own structured result JSON (parsed from
/// its masked `NativeToolEvent::result_text` — see [`extract_readonly_result_artifact`])
/// into the O-3 chat-artifact wire shape `{"type", "payload"}`
/// (`web/src/components/console/artifact-types.ts`). Fail-closed to `None`
/// whenever the tool's own result doesn't structurally carry what the
/// matching frontend card needs — this function never fabricates a field
/// the source JSON didn't actually have.
///
/// Per-tool payload shape (mirrors `artifact-types.ts` exactly, see that
/// file's own doc comments for why each shape looks the way it does):
///  - `os_device_status` → `device_status`: the tool's own return value
///    (`DeviceStatus`) IS `DeviceStatusArtifact.payload` already — passed
///    through unchanged.
///  - `os_network_info` → `network_info`: same — `{"interfaces": [...]}` is
///    already `NetworkInfoArtifact.payload` verbatim.
///  - `os_backup_list` → `backup_result`: the tool returns `{"files": [...]}`;
///    `BackupResultArtifact.payload` additionally needs a `mode` discriminant
///    (`"list"`, matching the RPC this card's other producer —
///    `device.backup_list` — would use) since the card also renders a
///    `"created"` mode this read-only path never produces.
///  - `os_check_update` → `update_status`: the tool returns
///    `{"system": {...}, "device": {...}}`; `UpdateStatusArtifact.payload` is
///    `{action, result?: DeviceOpResult, system?: SystemUpdateCheckPayload}`
///    (`result` is OPTIONAL — see below). The `device` half only ever
///    satisfies `DeviceOpResult {success, stdout, stderr}` on an appliance
///    install where `device_ops::update_status` actually ran
///    (`mcp_os_ops.rs`); an off-appliance install's `device` is shaped
///    `{"note": "..."}` and never populates `result`. Historically that made
///    a non-appliance install produce NO card at all, even though `system`
///    (the DuDuClaw self-version check, always attempted regardless of
///    `is_appliance()`) had a perfectly good answer — this function now
///    produces a **system-only** card (`result` omitted, `system` present)
///    whenever `device` doesn't parse but `system` does
///    ([`system_half_is_valid`]), so a non-appliance install still sees an
///    update card. `system`'s own `{"error": "..."}` failure shape is never
///    treated as "valid" — a system-only card would have nothing to show —
///    so device-invalid + system-error still correctly produces no card
///    (fail-closed, unchanged). When `device` IS valid, `system` is still
///    carried through verbatim regardless of its shape (including the
///    `{error}` shape) exactly as before — `UpdateStatusCard` already
///    tolerates that shape and renders a "check failed" note.
fn readonly_result_to_artifact(bare_tool: &str, result_json: &Value) -> Option<Value> {
    match bare_tool {
        "os_device_status" => Some(json!({ "type": "device_status", "payload": result_json })),
        "os_network_info" => {
            // Confirm the expected key exists before trusting the shape —
            // never guess.
            result_json.get("interfaces")?;
            Some(json!({ "type": "network_info", "payload": result_json }))
        }
        "os_backup_list" => {
            let files = result_json.get("files")?;
            Some(json!({
                "type": "backup_result",
                "payload": { "mode": "list", "files": files },
            }))
        }
        "os_check_update" => {
            // Device half: only ever satisfied by a real `DeviceOpResult`
            // (appliance install where `device_ops::update_status` ran).
            // `None` for a non-appliance `{"note": "..."}"` shape, a device
            // `{"error": "..."}"` shape, or a missing `device` key — never a
            // guess.
            let device_result = result_json.get("device").and_then(|device| {
                let success = device.get("success")?.as_bool()?;
                let stdout = device.get("stdout")?.as_str()?;
                let stderr = device.get("stderr")?.as_str()?;
                Some(json!({ "success": success, "stdout": stdout, "stderr": stderr }))
            });

            let system_raw = result_json.get("system");
            let system_is_valid = system_raw.is_some_and(system_half_is_valid);

            if device_result.is_none() && !system_is_valid {
                // Neither half is usable — fail closed, no card at all. This
                // is the only path that returns `None`: a device `{error}`/
                // `{note}` shape paired with a system `{error}`/missing shape
                // has nothing either section could render.
                return None;
            }

            let mut payload = json!({ "action": "check" });
            let obj = payload.as_object_mut().expect("json!({..}) is always an object");
            if let Some(result) = device_result {
                obj.insert("result".to_string(), result);
            }
            // Carry `system` (DuDuClaw self-version check) through verbatim
            // whenever present — including its `{error}` shape, as long as
            // SOMETHING justified producing a card (device valid above, or
            // `system_is_valid` above). `UpdateStatusCard` already tolerates
            // the `{error}` shape and renders a "check failed" note instead
            // of the version section.
            if let Some(system) = system_raw {
                obj.insert("system".to_string(), system.clone());
            }
            Some(json!({ "type": "update_status", "payload": payload }))
        }
        _ => None,
    }
}

/// Scan one reply turn's captured [`NativeToolEvent`]s (in call order) for
/// the LAST successful read-only `os_*` tool call whose result maps to an
/// O-3 artifact, and return that artifact.
///
/// Iterates newest-first: a later call in the same turn always wins over an
/// earlier one (the freshest status is what the user should see). An event
/// that doesn't qualify — wrong tool, a failed call, missing/unparseable
/// result text, or a result [`readonly_result_to_artifact`] doesn't know how
/// to render — is *skipped*, not treated as "give up": the scan keeps
/// looking further back, so an agent that calls `os_device_status` and then
/// a later, unrelated tool that happens to fail still gets its device-status
/// card. `None` when no qualifying event exists at all this turn.
///
/// Every step is fail-closed by construction (`?` / early `None` — never a
/// guess). `result_text` arrives already masked + char-capped by the time it
/// reaches [`NativeToolEvent`] (see that type's own doc comment), so this
/// function never masks or truncates anything itself — it only ever sees
/// what was already safe to keep.
pub fn extract_readonly_result_artifact(events: &[NativeToolEvent]) -> Option<Value> {
    events.iter().rev().find_map(|ev| {
        if !ev.success {
            return None;
        }
        let bare = readonly_result_tool_name(&ev.tool_name)?;
        let result_text = ev.result_text.as_deref()?;
        let result_json: Value = serde_json::from_str(result_text).ok()?;
        readonly_result_to_artifact(bare, &result_json)
    })
}

// ═══════════════════════════════════════════════════════════════════════
// T1 (`commercial/docs/DESIGN-agent-body-network-2026-08.md` §5.2/§12): the
// THIRD O-4→O-3 artifact source. Neither `marker_to_artifact` (a PENDING
// confirmation decided BEFORE any tool call, from static `OsIntentResult`
// params) nor `extract_readonly_result_artifact` (a SUCCESSFUL read-only
// tool result) fits this case: `os_wifi_connect` is a write tool, and the
// signal that matters here is one SPECIFIC FAILURE of that write —
// `wrong_password` — which means "hand off to a secure human-facing input
// surface", never "report the error and stop" (see `os_wifi_connect`'s own
// doc comment in `mcp_os_ops.rs` for why). This module still never calls an
// MCP tool handler or does any I/O — same guardrail as the rest of the
// file — it only reads back the already-masked `NativeToolEvent` evidence
// the CLI's own stream already captured.
// ═══════════════════════════════════════════════════════════════════════

/// Scan one turn's captured [`NativeToolEvent`]s for the LATEST
/// `os_wifi_connect` call, and — only if that latest attempt failed with
/// `wrong_password` — map it to the `wifi_password_request` O-3
/// chat-artifact (`web/src/components/console/WifiPasswordRequestCard.tsx`):
/// the card that lets a human type the passphrase directly into a masked
/// field wired to the `network.wifi_connect` dashboard RPC, never through
/// this agent's context (design §5.2). The payload carries ONLY `ssid` —
/// never a password, and never anything the original failed tool call
/// didn't already know structurally (`os_wifi_connect`'s MCP schema has no
/// `psk` field at all, so there is nothing secret in `ev.input_text` to
/// accidentally forward here even in principle).
///
/// Deliberately does NOT "keep scanning backward" past a non-matching
/// latest `os_wifi_connect` call the way [`extract_readonly_result_artifact`]
/// skips past an unrelated failed tool: only the FRESHEST connect attempt's
/// outcome is ever eligible. If that freshest attempt already succeeded (or
/// failed for a different reason), an EARLIER `wrong_password` failure in
/// the same turn must never resurface as a stale prompt — the human may
/// already be past that point. Every other tool call in the turn is simply
/// irrelevant to this extractor (it does not compete with
/// `extract_readonly_result_artifact` for "last qualifying event overall";
/// the caller in `channel_reply.rs` tries this extractor first and falls
/// back to the readonly one).
///
/// Fail-closed to `None` at every step (never guesses):
///  - no `os_wifi_connect` call this turn, or the latest one succeeded →
///    `None`.
///  - result text missing/unparseable, or its `code` isn't literally
///    `"wrong_password"` → `None`.
///  - input text missing/unparseable, or its `ssid` isn't a non-empty
///    string → `None` (never renders a card with no network name to show).
pub fn extract_wifi_password_request_artifact(events: &[NativeToolEvent]) -> Option<Value> {
    let last_connect = events.iter().rev().find(|ev| {
        let bare = ev.tool_name.rsplit("__").next().unwrap_or(&ev.tool_name);
        bare == "os_wifi_connect"
    })?;
    if last_connect.success {
        return None;
    }
    let result_text = last_connect.result_text.as_deref()?;
    let result_json: Value = serde_json::from_str(result_text).ok()?;
    if result_json.get("code").and_then(Value::as_str) != Some("wrong_password") {
        return None;
    }
    let input_text = last_connect.input_text.as_deref()?;
    let input_json: Value = serde_json::from_str(input_text).ok()?;
    let ssid = input_json.get("ssid").and_then(Value::as_str)?;
    if ssid.trim().is_empty() {
        return None;
    }
    Some(json!({
        "type": "wifi_password_request",
        "payload": { "ssid": ssid },
    }))
}

/// Build the directive appended to the dynamic (never-cached) tail of the
/// system prompt for a resolved, non-destructive `SystemOp` — guidance
/// only, never a substitute for the tool's own gate: if the tool call is
/// denied for any reason, the model is told to report that honestly rather
/// than improvise a workaround.
fn render_guide_hint(tool: OsTool, result: &OsIntentResult) -> String {
    format!(
        "## 系統操作意圖\n使用者這輪訊息判定為系統操作請求：{}。請呼叫 MCP 工具 `{}`\
         （參數：{}）完成請求，並用自然語言回報結果。若工具呼叫被拒絕，如實告知使用者原因，\
         絕不透過其他方式（例如 shell 指令）繞過。",
        tool_display_name(tool),
        tool.as_str(),
        result.params,
    )
}

// ═══════════════════════════════════════════════════════════════════════
// Audit
// ═══════════════════════════════════════════════════════════════════════

/// Append one `security_audit.jsonl` row for a `SystemOp`/`Rejected`
/// verdict — "誰用一句話做了什麼" stays reconstructable independent of the
/// O-0 tool's own `tool_calls.jsonl` record (which only fires if/when the
/// model actually makes the guided tool call). Never called for `Chat`/
/// `GoalTask` (the caller only invokes this alongside a non-`Continue`
/// [`OperatorAction`]), so ordinary conversation from an operator agent
/// does not flood the audit log.
pub fn audit_operator_decision(
    home_dir: &std::path::Path,
    agent_id: &str,
    text: &str,
    result: &OsIntentResult,
) {
    let severity = match result.category {
        OsIntentCategory::Rejected => duduclaw_security::audit::Severity::Warning,
        _ => duduclaw_security::audit::Severity::Info,
    };
    let details = json!({
        "category": format!("{:?}", result.category),
        "tool": result.tool.map(OsTool::as_str),
        "params": result.params,
        "needs_confirm": result.needs_confirm,
        "needs_approval": result.needs_approval,
        "reject_reason": result.reject_reason,
        "source": format!("{:?}", result.source),
        "signals": result.signals,
        "text_excerpt": duduclaw_core::truncate_chars(text, 200),
    });
    crate::security_autopilot::audit_and_emit(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "system_operator_intent",
            agent_id,
            severity,
            details,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os_intent::OsIntentSource;

    fn chat() -> OsIntentResult {
        OsIntentResult {
            category: OsIntentCategory::Chat,
            tool: None,
            params: json!({}),
            missing_params: vec![],
            needs_confirm: false,
            needs_approval: false,
            clarify_prompt: None,
            reject_reason: None,
            signals: vec![],
            source: OsIntentSource::L1,
        }
    }

    fn goal_task() -> OsIntentResult {
        OsIntentResult { category: OsIntentCategory::GoalTask, ..chat() }
    }

    fn rejected(reason: &str) -> OsIntentResult {
        OsIntentResult {
            category: OsIntentCategory::Rejected,
            reject_reason: Some(reason.to_string()),
            ..chat()
        }
    }

    fn system_op_missing() -> OsIntentResult {
        OsIntentResult {
            category: OsIntentCategory::SystemOp,
            tool: Some(OsTool::Power),
            params: json!({}),
            missing_params: vec!["action"],
            needs_confirm: true,
            needs_approval: false,
            clarify_prompt: Some("要重開機還是關機？".to_string()),
            reject_reason: None,
            signals: vec![],
            source: OsIntentSource::L1,
        }
    }

    fn system_op_ready_non_destructive() -> OsIntentResult {
        OsIntentResult {
            category: OsIntentCategory::SystemOp,
            tool: Some(OsTool::DeviceStatus),
            params: json!({}),
            missing_params: vec![],
            needs_confirm: false,
            needs_approval: false,
            clarify_prompt: None,
            reject_reason: None,
            signals: vec![],
            source: OsIntentSource::L1,
        }
    }

    fn system_op_ready_confirm() -> OsIntentResult {
        OsIntentResult {
            category: OsIntentCategory::SystemOp,
            tool: Some(OsTool::Power),
            params: json!({ "action": "restart" }),
            missing_params: vec![],
            needs_confirm: true,
            needs_approval: false,
            clarify_prompt: None,
            reject_reason: None,
            signals: vec![],
            source: OsIntentSource::L1,
        }
    }

    fn system_op_ready_approval() -> OsIntentResult {
        OsIntentResult {
            category: OsIntentCategory::SystemOp,
            tool: Some(OsTool::FactoryReset),
            params: json!({}),
            missing_params: vec![],
            needs_confirm: true,
            needs_approval: true,
            clarify_prompt: None,
            reject_reason: None,
            signals: vec![],
            source: OsIntentSource::L1,
        }
    }

    // ── decide(): Chat/GoalTask fall through unchanged ─────────────────────

    #[test]
    fn chat_continues() {
        assert_eq!(decide(&chat()), OperatorAction::Continue);
    }

    #[test]
    fn goal_task_continues() {
        assert_eq!(decide(&goal_task()), OperatorAction::Continue);
    }

    // ── decide(): Rejected short-circuits with the router's own reason ─────

    #[test]
    fn rejected_short_circuits_with_reject_reason() {
        let r = rejected("越界請求：要求執行任意系統指令。");
        match decide(&r) {
            OperatorAction::ShortCircuit(text) => {
                assert_eq!(text, "越界請求：要求執行任意系統指令。");
            }
            other => panic!("expected ShortCircuit, got {other:?}"),
        }
    }

    #[test]
    fn rejected_without_reason_still_short_circuits_with_a_message() {
        let mut r = rejected("");
        r.reject_reason = None;
        match decide(&r) {
            OperatorAction::ShortCircuit(text) => assert!(!text.is_empty()),
            other => panic!("expected ShortCircuit, got {other:?}"),
        }
    }

    // ── decide(): SystemOp with missing params → clarify, zero execution ───

    #[test]
    fn system_op_missing_params_short_circuits_with_clarify_prompt() {
        match decide(&system_op_missing()) {
            OperatorAction::ShortCircuit(text) => {
                assert_eq!(text, "要重開機還是關機？");
            }
            other => panic!("expected ShortCircuit, got {other:?}"),
        }
    }

    // ── decide(): SystemOp destructive → pending, NEVER auto-executed ──────

    #[test]
    fn system_op_needs_confirm_short_circuits_never_guides() {
        match decide(&system_op_ready_confirm()) {
            OperatorAction::ShortCircuit(text) => {
                assert!(text.contains("確認"), "must ask for confirmation: {text}");
                assert!(
                    text.contains(PENDING_TAG_OPEN),
                    "must carry the structured marker for O-3: {text}"
                );
            }
            other => panic!(
                "destructive SystemOp must NEVER be Guide (auto-execute risk), got {other:?}"
            ),
        }
    }

    #[test]
    fn system_op_needs_approval_short_circuits_mentions_approval() {
        match decide(&system_op_ready_approval()) {
            OperatorAction::ShortCircuit(text) => {
                assert!(text.contains("核准"), "must mention approval: {text}");
            }
            other => panic!("expected ShortCircuit, got {other:?}"),
        }
    }

    // ── decide(): SystemOp ready + non-destructive → Guide, continues pipeline ─

    #[test]
    fn system_op_ready_non_destructive_guides_with_tool_name_in_hint() {
        match decide(&system_op_ready_non_destructive()) {
            OperatorAction::Guide { tool, hint } => {
                assert_eq!(tool, OsTool::DeviceStatus);
                assert!(hint.contains("os_device_status"), "hint must name the tool: {hint}");
            }
            other => panic!("expected Guide, got {other:?}"),
        }
    }

    // ── strip_system_operator_pending_tag: round-trips render_pending's output ─

    #[test]
    fn pending_marker_round_trips() {
        let reply = render_pending(OsTool::Power, &system_op_ready_confirm());
        let (stripped, marker) = strip_system_operator_pending_tag(&reply);
        assert!(!stripped.contains(PENDING_TAG_OPEN));
        let marker = marker.expect("marker must parse");
        assert_eq!(marker["tool"], "os_power");
        assert_eq!(marker["needs_confirm"], true);
        assert_eq!(marker["needs_approval"], false);
    }

    #[test]
    fn strip_pending_tag_is_fail_open_on_no_tag() {
        let (stripped, marker) = strip_system_operator_pending_tag("ordinary reply, no tag here");
        assert_eq!(stripped, "ordinary reply, no tag here");
        assert!(marker.is_none());
    }

    #[test]
    fn strip_pending_tag_is_fail_open_on_unterminated_tag() {
        let text = format!("reply {PENDING_TAG_OPEN}not closed");
        let (stripped, marker) = strip_system_operator_pending_tag(&text);
        assert_eq!(stripped, text, "unterminated tag must leave the reply untouched");
        assert!(marker.is_none());
    }

    // ── marker_to_artifact: O-4→O-3 mapping, pure ───────────────────────────

    #[test]
    fn marker_to_artifact_os_power_restart_maps_to_confirm_action() {
        let marker = json!({
            "tool": "os_power",
            "params": { "action": "restart" },
            "needs_confirm": true,
            "needs_approval": false,
        });
        let artifact = marker_to_artifact(&marker).expect("must produce an artifact");
        assert_eq!(artifact["type"], "confirm_action");
        assert_eq!(artifact["payload"]["action"], "restart");
    }

    #[test]
    fn marker_to_artifact_os_power_shutdown_maps_to_confirm_action() {
        let marker = json!({
            "tool": "os_power",
            "params": { "action": "shutdown" },
            "needs_confirm": true,
            "needs_approval": false,
        });
        let artifact = marker_to_artifact(&marker).expect("must produce an artifact");
        assert_eq!(artifact["type"], "confirm_action");
        assert_eq!(artifact["payload"]["action"], "shutdown");
    }

    #[test]
    fn marker_to_artifact_os_power_unknown_action_is_none() {
        // Fail-closed: an action outside the closed {restart, shutdown} set
        // (malformed upstream data) never produces a half-guessed card.
        let marker = json!({ "tool": "os_power", "params": { "action": "sleep" } });
        assert!(marker_to_artifact(&marker).is_none());
    }

    #[test]
    fn marker_to_artifact_os_factory_reset_maps_to_confirm_action() {
        let marker = json!({
            "tool": "os_factory_reset",
            "params": {},
            "needs_confirm": true,
            "needs_approval": true,
        });
        let artifact = marker_to_artifact(&marker).expect("must produce an artifact");
        assert_eq!(artifact["type"], "confirm_action");
        assert_eq!(artifact["payload"]["action"], "factory_reset");
    }

    #[test]
    fn marker_to_artifact_os_apply_update_device_maps_to_update_confirm() {
        let marker = json!({
            "tool": "os_apply_update",
            "params": { "target": "device" },
            "needs_confirm": true,
            "needs_approval": false,
        });
        let artifact = marker_to_artifact(&marker).expect("must produce an artifact");
        assert_eq!(artifact["type"], "update_confirm");
        assert_eq!(artifact["payload"]["target"], "device");
    }

    #[test]
    fn marker_to_artifact_os_apply_update_system_maps_to_update_confirm() {
        let marker = json!({
            "tool": "os_apply_update",
            "params": { "target": "system" },
            "needs_confirm": true,
            "needs_approval": false,
        });
        let artifact = marker_to_artifact(&marker).expect("must produce an artifact");
        assert_eq!(artifact["type"], "update_confirm");
        assert_eq!(artifact["payload"]["target"], "system");
    }

    #[test]
    fn marker_to_artifact_os_apply_update_missing_target_is_none() {
        // Fail-closed: `decide()` never reaches `render_pending` with a
        // missing target in production (missing_params short-circuits to a
        // clarify prompt first), but this function never trusts that
        // invariant blindly — a marker without `params.target` still yields
        // no half-guessed card.
        let marker = json!({ "tool": "os_apply_update", "params": {} });
        assert!(marker_to_artifact(&marker).is_none());
    }

    #[test]
    fn marker_to_artifact_os_apply_update_unknown_target_is_none() {
        // Fail-closed: a target outside the closed {device, system} set
        // (malformed upstream data) never produces a half-guessed card.
        let marker = json!({ "tool": "os_apply_update", "params": { "target": "everything" } });
        assert!(marker_to_artifact(&marker).is_none());
    }

    #[test]
    fn marker_to_artifact_unknown_tool_is_none() {
        let marker = json!({ "tool": "os_device_status", "params": {} });
        assert!(marker_to_artifact(&marker).is_none());
    }

    #[test]
    fn marker_to_artifact_malformed_marker_is_none() {
        assert!(marker_to_artifact(&json!({})).is_none());
        assert!(marker_to_artifact(&json!({ "tool": 123 })).is_none());
        assert!(marker_to_artifact(&json!(null)).is_none());
    }

    // ── end-to-end: render_pending → strip → marker_to_artifact ────────────
    // This is the exact chain `channel_reply.rs` runs on every reply; these
    // tests double as the "marker never leaks" proof the caller needs: the
    // STRIPPED text is asserted to never carry the tag, for every case.

    #[test]
    fn pipeline_os_power_produces_stripped_text_and_confirm_action_artifact() {
        let reply = render_pending(OsTool::Power, &system_op_ready_confirm());
        let (stripped, marker) = strip_system_operator_pending_tag(&reply);
        assert!(!stripped.contains(PENDING_TAG_OPEN), "tag must never reach any channel");
        assert!(!stripped.contains(PENDING_TAG_CLOSE));
        let artifact = marker.as_ref().and_then(marker_to_artifact).expect("artifact expected");
        assert_eq!(artifact["type"], "confirm_action");
        assert_eq!(artifact["payload"]["action"], "restart");
    }

    #[test]
    fn pipeline_os_factory_reset_produces_stripped_text_and_confirm_action_artifact() {
        let reply = render_pending(OsTool::FactoryReset, &system_op_ready_approval());
        let (stripped, marker) = strip_system_operator_pending_tag(&reply);
        assert!(!stripped.contains(PENDING_TAG_OPEN), "tag must never reach any channel");
        let artifact = marker.as_ref().and_then(marker_to_artifact).expect("artifact expected");
        assert_eq!(artifact["type"], "confirm_action");
        assert_eq!(artifact["payload"]["action"], "factory_reset");
    }

    #[test]
    fn pipeline_os_apply_update_produces_stripped_text_and_update_confirm_artifact() {
        let mut result = system_op_ready_confirm();
        result.tool = Some(OsTool::ApplyUpdate);
        result.params = json!({ "target": "device" });
        let reply = render_pending(OsTool::ApplyUpdate, &result);
        let (stripped, marker) = strip_system_operator_pending_tag(&reply);
        assert!(!stripped.contains(PENDING_TAG_OPEN), "tag must never reach any channel");
        assert!(!stripped.contains(PENDING_TAG_CLOSE));
        let artifact = marker.as_ref().and_then(marker_to_artifact).expect("artifact expected");
        assert_eq!(artifact["type"], "update_confirm");
        assert_eq!(artifact["payload"]["target"], "device");
    }

    #[test]
    fn pipeline_plain_reply_without_marker_yields_no_artifact() {
        let reply = "一般聊天回覆，沒有任何 pending 標記。";
        let (stripped, marker) = strip_system_operator_pending_tag(reply);
        assert_eq!(stripped, reply);
        assert!(marker.is_none());
        assert!(marker.as_ref().and_then(marker_to_artifact).is_none());
    }

    // ── Task C: extract_readonly_result_artifact / readonly_result_to_artifact ─

    /// Test helper: a masked-text-carrying `NativeToolEvent` for a given
    /// (possibly `mcp__duduclaw__`-qualified) tool name and result text.
    fn native_ev(tool_name: &str, success: bool, result_text: Option<&str>) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool_name.to_string(),
            success,
            result_text: result_text.map(str::to_string),
            input_text: None,
        }
    }

    #[test]
    fn readonly_result_tool_name_matches_qualified_and_bare() {
        assert_eq!(
            readonly_result_tool_name("mcp__duduclaw__os_device_status"),
            Some("os_device_status")
        );
        assert_eq!(readonly_result_tool_name("os_device_status"), Some("os_device_status"));
    }

    #[test]
    fn readonly_result_tool_name_never_substring_matches() {
        // "os_device_status_extra" must NOT match "os_device_status" — exact
        // token equality only (project convention 2).
        assert!(readonly_result_tool_name("mcp__duduclaw__os_device_status_extra").is_none());
        assert!(readonly_result_tool_name("os_device_statusX").is_none());
    }

    #[test]
    fn readonly_result_tool_name_rejects_write_and_unrelated_tools() {
        assert!(readonly_result_tool_name("mcp__duduclaw__os_power").is_none());
        assert!(readonly_result_tool_name("mcp__duduclaw__os_apply_update").is_none());
        assert!(readonly_result_tool_name("Bash").is_none());
    }

    #[test]
    fn device_status_maps_result_through_unchanged() {
        let result = json!({ "cpu_cores": 8, "ram": { "total_mb": 16000 }, "network_interfaces": [] });
        let artifact = readonly_result_to_artifact("os_device_status", &result).unwrap();
        assert_eq!(artifact["type"], "device_status");
        assert_eq!(artifact["payload"], result);
    }

    #[test]
    fn network_info_maps_result_through_unchanged() {
        let result = json!({ "interfaces": [{ "name": "eth0", "is_up": true, "addresses": ["10.0.0.2"] }] });
        let artifact = readonly_result_to_artifact("os_network_info", &result).unwrap();
        assert_eq!(artifact["type"], "network_info");
        assert_eq!(artifact["payload"], result);
    }

    #[test]
    fn network_info_missing_interfaces_key_is_none() {
        assert!(readonly_result_to_artifact("os_network_info", &json!({})).is_none());
    }

    #[test]
    fn backup_list_wraps_files_with_list_mode() {
        let result = json!({ "files": [{ "name": "a.tar.gz", "size": 100, "mtime": 1 }] });
        let artifact = readonly_result_to_artifact("os_backup_list", &result).unwrap();
        assert_eq!(artifact["type"], "backup_result");
        assert_eq!(artifact["payload"]["mode"], "list");
        assert_eq!(artifact["payload"]["files"][0]["name"], "a.tar.gz");
    }

    #[test]
    fn backup_list_missing_files_key_is_none() {
        assert!(readonly_result_to_artifact("os_backup_list", &json!({})).is_none());
    }

    #[test]
    fn check_update_appliance_device_result_maps_to_update_status() {
        let result = json!({
            "system": { "available": true, "current_version": "1.0.0", "latest_version": "1.1.0" },
            "device": { "success": true, "stdout": "ok", "stderr": "" },
        });
        let artifact = readonly_result_to_artifact("os_check_update", &result).unwrap();
        assert_eq!(artifact["type"], "update_status");
        assert_eq!(artifact["payload"]["action"], "check");
        assert_eq!(artifact["payload"]["result"]["success"], true);
        assert_eq!(artifact["payload"]["result"]["stdout"], "ok");
        // The `system` half (DuDuClaw self-version check) is now carried
        // through verbatim so the card can show it alongside the appliance-OS
        // result — the device half still gates whether an artifact exists.
        assert_eq!(artifact["payload"]["system"]["available"], true);
        assert_eq!(artifact["payload"]["system"]["latest_version"], "1.1.0");
    }

    #[test]
    fn check_update_without_system_half_still_maps_device_only() {
        // A device-only result (no `system` key) still produces the artifact;
        // the `system` section is simply omitted, never fabricated.
        let result = json!({ "device": { "success": true, "stdout": "ok", "stderr": "" } });
        let artifact = readonly_result_to_artifact("os_check_update", &result).unwrap();
        assert_eq!(artifact["type"], "update_status");
        assert!(artifact["payload"].get("system").is_none());
    }

    #[test]
    fn check_update_non_appliance_incomplete_system_is_none() {
        // Off-appliance install, `device` is `{"note": "..."}"` (not
        // `DeviceOpResult`-shaped), AND `system` is structurally incomplete
        // (missing `current_version`/`latest_version` — not a real
        // `SystemUpdateCheckInfo`, e.g. a truncated/malformed payload). Both
        // halves unusable — must fail closed, never fabricate. Compare
        // `check_update_non_appliance_system_only_produces_system_only_card`
        // below for the P5 case where `system` IS complete.
        let result = json!({
            "system": { "available": false },
            "device": { "note": "非 appliance 安裝，無 OS image 更新可查" },
        });
        assert!(readonly_result_to_artifact("os_check_update", &result).is_none());
    }

    #[test]
    fn check_update_non_appliance_system_only_produces_system_only_card() {
        // P5: a non-appliance install (`device` shaped `{"note": "..."}"`,
        // never a `DeviceOpResult`) must still get an update card from the
        // `system` half (DuDuClaw's own version check, which runs regardless
        // of `is_appliance()`) — previously this fell through to `None`
        // entirely, so a non-appliance install NEVER saw an update card.
        let result = json!({
            "system": {
                "available": true,
                "current_version": "1.61.2",
                "latest_version": "1.62.0",
            },
            "device": { "note": "非 appliance 安裝，無 OS image 更新可查" },
        });
        let artifact = readonly_result_to_artifact("os_check_update", &result).unwrap();
        assert_eq!(artifact["type"], "update_status");
        assert_eq!(artifact["payload"]["action"], "check");
        // `result` (the device half) must be OMITTED, never fabricated as a
        // fake `DeviceOpResult` — the frontend contract makes it optional
        // exactly for this case (`UpdateStatusArtifact.payload.result?`).
        assert!(artifact["payload"].get("result").is_none());
        assert_eq!(artifact["payload"]["system"]["available"], true);
        assert_eq!(artifact["payload"]["system"]["latest_version"], "1.62.0");
    }

    #[test]
    fn check_update_missing_device_key_still_produces_system_only_card() {
        // Same as above but `device` is absent entirely (not even a `{note}`
        // shape) — must behave identically to the note-shape case.
        let result = json!({
            "system": {
                "available": false,
                "current_version": "1.62.0",
                "latest_version": "1.62.0",
            },
        });
        let artifact = readonly_result_to_artifact("os_check_update", &result).unwrap();
        assert_eq!(artifact["type"], "update_status");
        assert!(artifact["payload"].get("result").is_none());
        assert_eq!(artifact["payload"]["system"]["available"], false);
    }

    #[test]
    fn check_update_device_error_shape_with_valid_system_produces_system_only_card() {
        // A `device` RPC failure (`{"error": "..."}"`, e.g. duduclaw-sysd
        // unreachable on an appliance) must not blank out a perfectly good
        // `system` half either — same system-only path as the non-appliance
        // note shape.
        let result = json!({
            "system": {
                "available": true,
                "current_version": "1.0.0",
                "latest_version": "1.1.0",
            },
            "device": { "error": "duduclaw-sysd unreachable" },
        });
        let artifact = readonly_result_to_artifact("os_check_update", &result).unwrap();
        assert_eq!(artifact["type"], "update_status");
        assert!(artifact["payload"].get("result").is_none());
        assert_eq!(artifact["payload"]["system"]["available"], true);
    }

    #[test]
    fn check_update_system_error_alone_never_produces_a_card() {
        // P5 guardrail: a `system` half in its OWN `{"error": "..."}"`
        // failure shape must never, by itself, produce a system-only card —
        // there would be nothing for the card to show. Paired here with a
        // device half that is ALSO unusable (the only way `None` can result
        // post-P5); a device-valid pairing is covered by
        // `check_update_appliance_device_result_maps_to_update_status`,
        // which still carries an error-shaped `system` through as the
        // enrichment field precisely because `device` alone already justified
        // the card.
        let result = json!({ "system": { "error": "boom" }, "device": { "error": "boom" } });
        assert!(readonly_result_to_artifact("os_check_update", &result).is_none());
    }

    #[test]
    fn check_update_both_halves_missing_is_none() {
        assert!(readonly_result_to_artifact("os_check_update", &json!({})).is_none());
    }

    #[test]
    fn extract_picks_last_qualifying_event_newest_first() {
        let events = vec![
            native_ev(
                "mcp__duduclaw__os_device_status",
                true,
                Some(r#"{"cpu_cores":4,"network_interfaces":[]}"#),
            ),
            native_ev(
                "mcp__duduclaw__os_network_info",
                true,
                Some(r#"{"interfaces":[]}"#),
            ),
        ];
        let artifact = extract_readonly_result_artifact(&events).unwrap();
        // The LAST event (os_network_info) wins over the earlier device_status.
        assert_eq!(artifact["type"], "network_info");
    }

    #[test]
    fn extract_skips_failed_call_and_keeps_scanning_backward() {
        let events = vec![
            native_ev(
                "mcp__duduclaw__os_device_status",
                true,
                Some(r#"{"cpu_cores":4,"network_interfaces":[]}"#),
            ),
            // A later, unrelated tool that failed must not blank out the
            // earlier successful device-status result.
            native_ev("Bash", false, Some("permission denied")),
        ];
        let artifact = extract_readonly_result_artifact(&events).unwrap();
        assert_eq!(artifact["type"], "device_status");
    }

    #[test]
    fn extract_failed_readonly_call_itself_is_skipped() {
        let events = vec![native_ev(
            "mcp__duduclaw__os_device_status",
            false,
            Some(r#"{"cpu_cores":4}"#),
        )];
        assert!(extract_readonly_result_artifact(&events).is_none());
    }

    #[test]
    fn extract_missing_result_text_is_none() {
        let events = vec![native_ev("mcp__duduclaw__os_device_status", true, None)];
        assert!(extract_readonly_result_artifact(&events).is_none());
    }

    #[test]
    fn extract_unparseable_result_text_is_none() {
        // Simulates a truncated (`NATIVE_EVENT_RESULT_MAX_CHARS`-capped) JSON
        // payload — malformed, not a crash, never a partial-artifact guess.
        let events = vec![native_ev(
            "mcp__duduclaw__os_device_status",
            true,
            Some(r#"{"cpu_cores":4,"network_interf"#),
        )];
        assert!(extract_readonly_result_artifact(&events).is_none());
    }

    #[test]
    fn extract_unrelated_tool_only_is_none() {
        let events = vec![native_ev("Read", true, Some("file contents"))];
        assert!(extract_readonly_result_artifact(&events).is_none());
    }

    #[test]
    fn extract_empty_events_is_none() {
        assert!(extract_readonly_result_artifact(&[]).is_none());
    }

    #[test]
    fn extract_destructive_os_tool_never_produces_an_artifact() {
        // Even if a write tool somehow left native evidence in this turn
        // (should never happen — O-4 never lets a destructive tool auto-run),
        // this path must never turn it into a card.
        let events = vec![native_ev(
            "mcp__duduclaw__os_power",
            true,
            Some(r#"{"success":true,"stdout":"","stderr":""}"#),
        )];
        assert!(extract_readonly_result_artifact(&events).is_none());
    }

    // ── audit_operator_decision: writes a row, never panics ────────────────

    #[test]
    fn audit_writes_one_row_for_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let r = rejected("越界請求");
        audit_operator_decision(tmp.path(), "sysop", "sudo rm -rf /", &r);

        let events = duduclaw_security::audit::read_recent_events(tmp.path(), 10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "system_operator_intent");
        assert_eq!(events[0].agent_id, "sysop");
    }

    // ── T1: extract_wifi_password_request_artifact ─────────────────────────

    /// Test helper: a masked-text-carrying `NativeToolEvent` that also sets
    /// `input_text` (unlike [`native_ev`] above, which always leaves it
    /// `None` — none of the pre-T1 tests needed a tool CALL's arguments).
    fn native_ev_with_input(
        tool_name: &str,
        success: bool,
        result_text: Option<&str>,
        input_text: Option<&str>,
    ) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool_name.to_string(),
            success,
            result_text: result_text.map(str::to_string),
            input_text: input_text.map(str::to_string),
        }
    }

    #[test]
    fn wifi_password_request_fires_on_wrong_password() {
        let events = vec![native_ev_with_input(
            "mcp__duduclaw__os_wifi_connect",
            false,
            Some(r#"{"code":"wrong_password","message":"密碼不正確，請重新輸入"}"#),
            Some(r#"{"ssid":"iPhone-Sam","confirm":true}"#),
        )];
        let artifact = extract_wifi_password_request_artifact(&events).unwrap();
        assert_eq!(artifact["type"], "wifi_password_request");
        assert_eq!(artifact["payload"]["ssid"], "iPhone-Sam");
        // No other field — never a psk, never a guessed security label.
        assert_eq!(artifact["payload"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn wifi_password_request_bare_name_matches_unqualified_too() {
        let events = vec![native_ev_with_input(
            "os_wifi_connect",
            false,
            Some(r#"{"code":"wrong_password","message":"x"}"#),
            Some(r#"{"ssid":"DuDu-Guest"}"#),
        )];
        assert!(extract_wifi_password_request_artifact(&events).is_some());
    }

    #[test]
    fn wifi_password_request_never_fires_on_other_error_codes() {
        for code in ["out_of_range", "not_found", "backend_unavailable", "no_adapter"] {
            let events = vec![native_ev_with_input(
                "mcp__duduclaw__os_wifi_connect",
                false,
                Some(&format!(r#"{{"code":"{code}","message":"x"}}"#)),
                Some(r#"{"ssid":"DuDu-Office"}"#),
            )];
            assert!(
                extract_wifi_password_request_artifact(&events).is_none(),
                "code {code} must never trigger a password prompt"
            );
        }
    }

    #[test]
    fn wifi_password_request_never_fires_on_success() {
        let events = vec![native_ev_with_input(
            "mcp__duduclaw__os_wifi_connect",
            true,
            Some(r#"{"state":"connected","ssid":"DuDu-Office"}"#),
            Some(r#"{"ssid":"DuDu-Office"}"#),
        )];
        assert!(extract_wifi_password_request_artifact(&events).is_none());
    }

    #[test]
    fn wifi_password_request_missing_ssid_is_none() {
        let events = vec![native_ev_with_input(
            "mcp__duduclaw__os_wifi_connect",
            false,
            Some(r#"{"code":"wrong_password","message":"x"}"#),
            Some(r#"{"confirm":true}"#), // no ssid — malformed/truncated input
        )];
        assert!(extract_wifi_password_request_artifact(&events).is_none());
    }

    #[test]
    fn wifi_password_request_missing_input_text_is_none() {
        let events = vec![native_ev_with_input(
            "mcp__duduclaw__os_wifi_connect",
            false,
            Some(r#"{"code":"wrong_password","message":"x"}"#),
            None,
        )];
        assert!(extract_wifi_password_request_artifact(&events).is_none());
    }

    #[test]
    fn wifi_password_request_ignores_smuggled_psk_in_input_text() {
        // Defence in depth: even if some future bug let a `psk`-shaped key
        // leak into the masked input text, this extractor must never read
        // or forward it — only `ssid` is ever pulled out.
        let events = vec![native_ev_with_input(
            "mcp__duduclaw__os_wifi_connect",
            false,
            Some(r#"{"code":"wrong_password","message":"x"}"#),
            Some(r#"{"ssid":"iPhone-Sam","psk":"should-never-appear"}"#),
        )];
        let artifact = extract_wifi_password_request_artifact(&events).unwrap();
        assert!(!artifact.to_string().contains("should-never-appear"));
        assert_eq!(artifact["payload"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn wifi_password_request_only_considers_the_latest_connect_attempt() {
        // A stale earlier failure must never resurface once a LATER attempt
        // in the same turn settled (success here) — see this function's own
        // doc comment for why it deliberately does NOT "keep scanning
        // backward" past a non-matching latest attempt.
        let events = vec![
            native_ev_with_input(
                "mcp__duduclaw__os_wifi_connect",
                false,
                Some(r#"{"code":"wrong_password","message":"x"}"#),
                Some(r#"{"ssid":"iPhone-Sam"}"#),
            ),
            native_ev_with_input(
                "mcp__duduclaw__os_wifi_connect",
                true,
                Some(r#"{"state":"connected","ssid":"DuDu-Office"}"#),
                Some(r#"{"ssid":"DuDu-Office"}"#),
            ),
        ];
        assert!(extract_wifi_password_request_artifact(&events).is_none());
    }

    #[test]
    fn wifi_password_request_empty_events_is_none() {
        assert!(extract_wifi_password_request_artifact(&[]).is_none());
    }

    #[test]
    fn wifi_password_request_unrelated_tool_only_is_none() {
        let events = vec![native_ev_with_input("Bash", false, Some("x"), Some("{}"))];
        assert!(extract_wifi_password_request_artifact(&events).is_none());
    }
}
