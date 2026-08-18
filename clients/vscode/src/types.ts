// Shared payload shapes for gateway responses this extension consumes.
// Field names are copied verbatim from the Rust source (not guessed) —
// see the comment above each interface for the exact origin.

/** `AccessLevel` — `crates/duduclaw-auth/src/models.rs`, `#[serde(rename_all
 * = "lowercase")]`. Full control / operational / read-only, in that order. */
export type AccessLevel = 'owner' | 'operator' | 'viewer';

/** `UserRole` — same file, same serde rename. Dashboard/account-wide role,
 * distinct from an agent's OWN `role` field (e.g. "worker") in
 * `AgentListEntry` below — never conflate the two. */
export type UserRole = 'admin' | 'manager' | 'employee';

/**
 * One row of the `agents.list` RPC response
 * (`crates/duduclaw-gateway/src/handlers.rs::handle_agents_list_filtered`,
 * ~line 24740). The server already filters this list by `ctx.visible_agents()`
 * (binding-based for non-admins, unfiltered for admins) — the client does no
 * additional access filtering, only display.
 *
 * IMPORTANT: this payload does NOT carry a per-agent access level for the
 * calling user. That only exists in `GET /api/me`'s `bindings` array
 * (`UserAgentBinding.access_level`) and must be joined in by `agent_name` —
 * see `mergeAgentOptions` in panel.ts. Do not invent an `access_level` field
 * here; it is not on the wire.
 */
export interface AgentListEntry {
  name: string;
  display_name?: string | null;
  role?: string;
  status?: string;
  archived?: boolean;
  department?: string | null;
  icon?: string | null;
  // agents.list carries many more fields (model/budget/heartbeat/skills/...)
  // this client has no use for; keep the type open rather than guess them.
  [key: string]: unknown;
}

/** `UserAgentBinding` — `crates/duduclaw-auth/src/models.rs`. */
export interface UserAgentBinding {
  user_id: string;
  agent_name: string;
  access_level: AccessLevel;
  bound_at: string;
}

/** `GET /api/me` response — `handle_me` in `crates/duduclaw-gateway/src/server.rs`
 * (~line 2783): `{"user": User, "bindings": UserAgentBinding[]}`. */
export interface MeResponse {
  user: {
    id: string;
    email: string;
    display_name: string;
    role: UserRole;
    status: string;
  };
  bindings: UserAgentBinding[];
}

/** An agent as offered to the user in the picker, after joining
 * `agents.list` with `/api/me`'s bindings for the access-level badge.
 * `accessLevel` is `undefined` for admins (who bypass per-agent bindings
 * entirely — `UserContext::visible_agents()` returns `None` for them) and
 * for any agent the current `/api/me` call didn't resolve a binding for. */
export interface AgentOption {
  name: string;
  displayName: string;
  accessLevel: AccessLevel | undefined;
}

/** Pushed from `DuduPanelProvider` to `extension.ts` whenever auth/agent/role
 * state changes, to drive the native VS Code status bar item. */
export interface PanelStatusState {
  authed: boolean;
  agentName?: string;
  role?: UserRole;
}

/** Composer mode (design doc §4 P1 "交辦／想一想模式"). `ask` is the
 * pre-P1 plain-chat behaviour; `delegate`/`plan` route through
 * `tasks.goal_create` instead of the WebChat socket. */
export type ChatMode = 'ask' | 'delegate' | 'plan';

/** H11 pause-reason token — `crates/duduclaw-gateway/src/pause_reason.rs`
 * `PauseReason::as_str()`. Always resolved server-side (a legacy/unknown row
 * still arrives as `"unknown"`, never absent) but only meaningful while
 * `status === 'needs_human'`. */
export type PauseReasonToken =
  | 'no_progress'
  | 'budget_exhausted'
  | 'blocked_needs_decision'
  | 'infra'
  | 'restart'
  | 'unknown';

/**
 * One row of the `tasks.list` RPC response
 * (`crates/duduclaw-gateway/src/handlers.rs::task_row_to_json`, ~line 32512).
 * Only the fields this panel renders are named; the server returns many more
 * (goal_state, deadline_at, revision_round, ...) — kept open rather than
 * guessed at.
 */
export interface TaskItem {
  id: string;
  title: string;
  description: string;
  status: string;
  priority?: string;
  assigned_to: string;
  judge_feedback?: string | null;
  pause_reason?: PauseReasonToken | null;
  plan_pending?: string | null;
  goal_mode?: boolean;
  updated_at?: string;
  [key: string]: unknown;
}

/**
 * `tasks.goal_create` RPC response
 * (`handle_tasks_goal_create` in `crates/duduclaw-gateway/src/handlers.rs`,
 * ~line 28765): `{task, iteration_cap, dispatch_enabled, plan_first}`.
 */
export interface GoalCreateResult {
  task: TaskItem;
  iteration_cap: number;
  dispatch_enabled: boolean;
  /** Echoes the request's `plan_first` — lets the caller show "計畫已生成，
   * 等待核准" without re-deriving it from `task.status`/`plan_pending`. */
  plan_first: boolean;
}
