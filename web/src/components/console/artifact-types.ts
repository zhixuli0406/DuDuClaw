import type {
  DeviceStatus,
  DeviceNetworkInterface,
  DeviceOpResult,
  DeviceBackupResult,
  DeviceBackupFileEntry,
  ApprovalItem,
} from '@/lib/api';

/**
 * Chat artifact contract (O-3, `commercial/docs/DESIGN-agent-os-native-apps-
 * 2026-08.md` §6.2 UI/UX + §6.3 O-3). A "chat artifact" is a structured
 * result or confirmation request an agent reply carries alongside its plain
 * text so the operator console (`OperatorConsolePage`, O-2) renders it as a
 * card in the message flow — "結果內嵌、不跳 app" — instead of the user having
 * to open 裝置/系統設定 to see the same information.
 *
 * Design choices that matter for whoever wires O-1/O-4 into this:
 *  - Every variant is `{ type, payload }`. `type` selects the card component
 *    via `ChatArtifactCard`'s dispatch; `payload` reuses the SAME wire shape
 *    the corresponding `device.*` / `approvals.*` RPC already returns (see
 *    `@/lib/api`) wherever one exists — no new backend serialization format
 *    is invented here, O-1/O-4 only need to attach the RPC result they
 *    already have in hand after calling the O-0 tool.
 *  - This module intentionally has ZERO runtime code — it is a pure type
 *    contract. The actual gate/confirm/approve logic each card enforces
 *    lives in the card component itself and always goes through the same
 *    gated `api.device.*` / `api.approvals.*` calls DevicePage/ApprovalsPage
 *    use (see coding convention #4 in CLAUDE.md: UI never invents its own
 *    authority — it only triggers the existing gate).
 *
 * WIRING STATUS: `ChatMessage.artifact` IS populated over the wire as of the
 * O-4→O-3 pass — `assistant_done` (see `stores/chat-store.ts`'s
 * `parseChatArtifact`) carries an optional `artifact` field the gateway sets
 * from two independent sources in `crates/duduclaw-gateway/src/os_operator.rs`:
 *  - `marker_to_artifact` maps a reply's `<system_operator_pending>` marker
 *    (a destructive op awaiting confirmation) to `confirm_action` (pending
 *    restart/shutdown/factory-reset) or `update_confirm` (pending
 *    `os_apply_update`, device/system).
 *  - `extract_readonly_result_artifact` (Task C) maps the LAST successful
 *    read-only `os_*` tool call a `system_operator` agent actually made this
 *    turn to `device_status` / `update_status` / `backup_result` /
 *    `network_info` — the RESULT half, as opposed to `marker_to_artifact`'s
 *    PENDING-confirmation half. `approval_request` still has no production
 *    producer and remains proven only against `./fixtures.ts` and component
 *    tests. `assistant_chunk` (streaming) and `historyToMessages`
 *    (resumed-conversation history) still never set it — only the settled
 *    `assistant_done` frame on a live socket does.
 */

export type ChatArtifactType =
  | 'device_status'
  | 'update_status'
  | 'backup_result'
  | 'network_info'
  | 'confirm_action'
  | 'update_confirm'
  | 'approval_request';

interface ChatArtifactBase<T extends ChatArtifactType> {
  readonly type: T;
  /** Stable id for React keys / de-dupe across re-renders. Falls back to the
   *  owning `ChatMessage.id` when the producer doesn't mint one. */
  readonly id?: string;
}

/** `device.status` result rendered as a compact "裝置狀態" card. */
export interface DeviceStatusArtifact extends ChatArtifactBase<'device_status'> {
  readonly payload: DeviceStatus;
}

/** The `system` half of `os_check_update`'s combined result (self-update
 *  version-check info, i.e. whether DuDuClaw itself — not the appliance OS
 *  image — has a newer release). A trimmed mirror of the dashboard
 *  `system.checkUpdate()` RPC shape (`@/lib/api`), matching exactly what
 *  `mcp_os_ops.rs`'s `handle_os_check_update` puts under its `system` key
 *  (deliberately narrower: no `download_url`/`checksum_url`, see that
 *  function's own doc comment for why). `{ error }` is the shape when the
 *  underlying `updater::check_update()` call itself failed. */
export interface SystemUpdateCheckInfo {
  readonly available: boolean;
  readonly current_version: string;
  readonly latest_version: string;
  readonly release_notes?: string | null;
  readonly published_at?: string | null;
  readonly install_method?: string | null;
  readonly containerized?: boolean;
}

export type SystemUpdateCheckPayload = SystemUpdateCheckInfo | { readonly error: string };

/** `device.update_status` / `device.update_apply` result. `action` picks the
 *  card's heading ("已檢查更新" vs "已套用更新") — the RPC result shape
 *  (`DeviceOpResult`) doesn't otherwise say which one produced it.
 *
 *  `result` (the appliance-OS `device` half) is OPTIONAL (P5): a non-appliance
 *  install never has a `DeviceOpResult` to show (`os_check_update`'s `device`
 *  half is a `{"note": "..."}"` shape there, never a real op result), but its
 *  `system` half (DuDuClaw's own version check) still runs and still has a
 *  useful answer — `os_operator.rs`'s `readonly_result_to_artifact` now
 *  produces a **system-only** card in that case (`result` omitted, `system`
 *  present) instead of no card at all. `system` is itself optional too — a
 *  device-valid card omits it exactly when the backend's `os_check_update`
 *  result carried no `system` key. At least one of `result`/`system` is
 *  always present in a real backend-produced payload (the backend fails
 *  closed to no artifact when neither half is usable); the card renders
 *  whichever half(s) are present and never fabricates the other. */
export interface UpdateStatusArtifact extends ChatArtifactBase<'update_status'> {
  readonly payload: {
    readonly action: 'check' | 'apply';
    readonly result?: DeviceOpResult;
    readonly system?: SystemUpdateCheckPayload;
  };
}

/** `device.backup_create` (a single fresh archive with a download link) or
 *  `device.backup_list` (the existing archive list) — one card, two render
 *  modes, matching the two DevicePage 備份卡 states. */
export type BackupResultPayload =
  | { readonly mode: 'created'; readonly result: DeviceBackupResult }
  | { readonly mode: 'list'; readonly files: readonly DeviceBackupFileEntry[] };

export interface BackupResultArtifact extends ChatArtifactBase<'backup_result'> {
  readonly payload: BackupResultPayload;
}

/** `device.network` result. */
export interface NetworkInfoArtifact extends ChatArtifactBase<'network_info'> {
  readonly payload: { readonly interfaces: readonly DeviceNetworkInterface[] };
}

/** The three destructive `device.*` actions DevicePage's DangerZone gates
 *  behind a modal `ConfirmDialog`. O-3 embeds the SAME gate inline in the
 *  chat flow instead (see `ConfirmActionCard`) — type "RESET" for
 *  `factory_reset` (irreversible), a short mandatory countdown for
 *  `restart` / `shutdown` (disruptive but not data-destroying). */
export type ConfirmActionKind = 'restart' | 'shutdown' | 'factory_reset';

export interface ConfirmActionPayload {
  readonly action: ConfirmActionKind;
}

export interface ConfirmActionArtifact extends ChatArtifactBase<'confirm_action'> {
  readonly payload: ConfirmActionPayload;
}

/** A pending `os_apply_update` (O-0) awaiting the user's explicit go-ahead,
 *  rendered as `UpdateConfirmCard` — the non-destructive twin of
 *  `ConfirmActionCard`: updating is recoverable (unlike `factory_reset`) and
 *  disruption is brief (unlike `restart`/`shutdown`'s "every service drops"),
 *  so this card needs neither a typed "RESET" nor a mandatory countdown —
 *  one explicit click confirms. `target` picks which RPC the confirm click
 *  calls: `device` → `api.device.updateApply()` (appliance OS image via
 *  duduclaw-sysd), `system` → `api.system.applyUpdate()` (duduclaw
 *  self-update) — mirrors the two RPCs `os_apply_update` itself bridges (see
 *  `mcp_os_ops.rs`'s doc comment). */
export type UpdateConfirmTarget = 'device' | 'system';

export interface UpdateConfirmPayload {
  readonly target: UpdateConfirmTarget;
}

export interface UpdateConfirmArtifact extends ChatArtifactBase<'update_confirm'> {
  readonly payload: UpdateConfirmPayload;
}

/** One `ApprovalItem` (the same shape `approvals.list` / the `ApprovalModal`
 *  push already use) rendered as an inline approve/reject card instead of a
 *  full-screen modal or a trip to `/inbox`. Deciding calls the SAME
 *  `approvals.decide` RPC — ApprovalBroker's TTL/fail-closed semantics are
 *  untouched (coding convention #4: gates fail closed, this UI only adds a
 *  front door). */
export interface ApprovalRequestArtifact extends ChatArtifactBase<'approval_request'> {
  readonly payload: ApprovalItem;
}

/** The full discriminated union `ChatArtifactCard` dispatches on. */
export type ChatArtifact =
  | DeviceStatusArtifact
  | UpdateStatusArtifact
  | BackupResultArtifact
  | NetworkInfoArtifact
  | ConfirmActionArtifact
  | UpdateConfirmArtifact
  | ApprovalRequestArtifact;
