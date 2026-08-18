// Host-side gateway sockets: WebChat chat stream + dashboard JSON-RPC.
//
// Architecture note (Origin gate): the gateway rejects browser Origins it
// doesn't know, and VS Code webviews carry a RANDOM `vscode-webview://<uuid>`
// Origin — unallowlistable. So ALL sockets live in the extension host
// (Node `ws` sends no Origin header ⇒ allowed as a non-browser client), and
// the webview is pure UI talking to the host via postMessage. A side benefit:
// the JWT never enters the webview at all.
import * as vscode from 'vscode';
import WebSocket from 'ws';
import { SECRET_ACCESS, gatewayUrl, sessionStateKey } from './config';
import { friendlyErrorMessage, isAuthError, refreshAccessToken } from './auth';
import type { AgentListEntry, GoalCreateResult, TaskItem } from './types';

export const NOT_AUTHED_ERROR = 'not-authed';

export type Post = (msg: unknown) => void;

export class GatewayClient {
  private chat: WebSocket | undefined;
  private rpcSock: WebSocket | undefined;
  private rpcReady: Promise<void> | undefined;
  private rpcSeq = 0;
  private pending = new Map<string, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  /** The AI employee `user_message` frames are addressed to. `undefined` ⇒
   * server default/main-agent behaviour (byte-compatible pre-v2 path). */
  private currentAgent: string | undefined;

  // ── WP4 session resume (design doc §4 P1) ──
  // See `sendChat` and the chat socket's `message` handler for the full
  // state machine. Summary: `connectionBaseSid` is the CURRENT connection's
  // own agent-less session id (from the connect-time `session_info` frame);
  // `activeSid` is whatever session id is currently addressing `currentAgent`
  // on THIS connection — once set, every subsequent `user_message` this
  // connection reuses it so history stays in one bucket. Both reset to
  // `undefined` whenever a fresh chat socket is created (new connection ⇒ a
  // new random connection suffix server-side, so any previously-known id
  // must be re-validated via an explicit resume, never assumed).
  private connectionBaseSid: string | undefined;
  private activeSid: string | undefined;
  /** True from the moment we send a `user_message` carrying an explicit
   * `session_id` (a resume attempt) until we see either a confirming
   * `session_info` echo or an `assistant_done` — used ONLY to decide whether
   * the NEXT `error` frame should be treated as "resume failed, reset"
   * (deliberately not string-matching the gateway's plain-text resume
   * failure reasons — see the message handler). */
  private resumeAttempted = false;

  constructor(
    private readonly ctx: vscode.ExtensionContext,
    private readonly post: Post
  ) {}

  dispose(): void {
    try { this.chat?.close(); } catch { /* noop */ }
    try { this.rpcSock?.close(); } catch { /* noop */ }
    this.chat = undefined;
    this.rpcSock = undefined;
    this.rpcReady = undefined;
  }

  private wsUrl(path: string): string {
    return gatewayUrl().replace(/^http/, 'ws') + path;
  }

  private async token(): Promise<string | undefined> {
    return this.ctx.secrets.get(SECRET_ACCESS);
  }

  /**
   * Switches the active agent for future chat turns. Closes any live chat
   * socket so a reply already in flight for the PREVIOUS agent can never
   * render after the switch — the `message` handler below double-checks
   * `this.chat === sock` before posting anything, so even a frame that was
   * already queued in the event loop when `close()` was called is dropped.
   * This is the simplest way to guarantee per-agent conversations never
   * cross-contaminate without threading a conversation-id through every
   * WebChat frame type (several, like `assistant_chunk`, don't carry one).
   */
  switchAgent(agentName: string | undefined): void {
    this.currentAgent = agentName;
    try { this.chat?.close(); } catch { /* noop */ }
    this.chat = undefined;
    // A different agent (or a fresh reconnect to the same one) always talks
    // through a brand-new socket — any session id bookkeeping tied to the
    // OLD connection is meaningless for it. `ensureChat` will re-derive
    // `connectionBaseSid` from the new connect frame; `activeSid` is looked
    // up fresh from persisted state (or left unset) on the next `sendChat`.
    this.connectionBaseSid = undefined;
    this.activeSid = undefined;
    this.resumeAttempted = false;
  }

  get agent(): string | undefined {
    return this.currentAgent;
  }

  // ── chat (WebChat protocol) ──
  private async ensureChat(retried = false): Promise<WebSocket> {
    if (this.chat && this.chat.readyState === WebSocket.OPEN) return this.chat;
    const token = await this.token();
    if (!token) throw new Error(NOT_AUTHED_ERROR);
    const sock = new WebSocket(this.wsUrl('/ws/chat'));
    this.chat = sock;
    // Fresh socket ⇒ fresh connection ⇒ the gateway will mint a NEW random
    // connection suffix for its base session id (`webchat.rs` `handle_chat_socket`
    // — `session_suffix = truncate_bytes(&session_uuid, 8)` per connection).
    // Any id captured on the PREVIOUS socket no longer describes this one.
    this.connectionBaseSid = undefined;
    this.activeSid = undefined;
    this.resumeAttempted = false;
    // Resolves once the connect-time `session_info` frame (always the first
    // frame the server sends after a successful auth handshake) has been
    // captured into `connectionBaseSid` — see the `message` handler below.
    // Bounded so a slow/missing frame degrades to "no resume this
    // connection" instead of blocking chat indefinitely.
    let sawConnectFrame = false;
    let resolveConnectFrame: (() => void) | undefined;
    const connectFrameSeen = new Promise<void>((resolve) => {
      resolveConnectFrame = resolve;
    });
    sock.on('message', async (raw: Buffer) => {
      // Stale socket from a prior agent switch — its close() may not have
      // fully unwound before this queued message event fired. Drop it.
      if (this.chat !== sock) return;
      let m: { type?: string; message?: string; session_id?: string; refresh?: boolean; [k: string]: unknown };
      try { m = JSON.parse(raw.toString('utf8')); } catch { return; }

      // ── WP4 resume bookkeeping ──
      // `session_info` frames arrive in exactly three shapes (`webchat.rs`):
      // (1) the connect-time announcement — always first, always agent-less,
      //     `refresh: false`; (2) a resume confirmation — ONLY sent when the
      //     client's `user_message.session_id` named an existing session,
      //     `refresh: false`; (3) an agent-config-change broadcast,
      //     `refresh: true`, carrying no new session identity. (1) and (2)
      //     are told apart purely by ORDER (the connect frame is guaranteed
      //     first — the server sends it synchronously right after the auth
      //     handshake, before entering the per-message loop), not by content.
      if (m.type === 'session_info' && typeof m.session_id === 'string') {
        if (!sawConnectFrame) {
          sawConnectFrame = true;
          this.connectionBaseSid = m.session_id;
          resolveConnectFrame?.();
        } else if (!m.refresh) {
          this.activeSid = m.session_id;
          this.resumeAttempted = false;
          this.persistSid(m.session_id);
        }
      }

      if (
        m.type === 'error' &&
        isAuthError(m.message ?? '') &&
        !retried
      ) {
        const t = await refreshAccessToken(this.ctx);
        sock.close();
        if (this.chat === sock) this.chat = undefined;
        if (t) {
          try { await this.ensureChat(true); } catch { this.post({ type: 'auth-required' }); }
        } else {
          this.post({ type: 'auth-required' });
        }
        return;
      }

      if (m.type === 'error') {
        // Any error following a resume attempt (we sent an explicit
        // `session_id`) means the resume didn't take. Deliberately NOT
        // string-matching the specific reason — "conversation not found" /
        // "this conversation belongs to a different AI staff member" / "the
        // AI staff member for this conversation is no longer available" are
        // plain English literals in `webchat.rs`, not a stable `code` like
        // `agent_forbidden` — matching them would be exactly the brittle
        // unanchored-text coupling project convention #2 warns against. Fail
        // open instead: drop the stale id so the NEXT message starts a fresh
        // bucket rather than repeating the same failure forever.
        if (this.resumeAttempted) {
          this.resumeAttempted = false;
          this.activeSid = undefined;
          this.clearPersistedSid();
        }
      } else if (m.type === 'assistant_done') {
        this.resumeAttempted = false;
        if (!this.activeSid && this.connectionBaseSid) {
          // First successful reply on this connection with no resume in
          // play ⇒ the gateway just organically composed a brand-new bucket
          // for `currentAgent` via `compose_session_id(base, agent, None)`
          // (`webchat.rs`) — `{base}` + `#agent:{name}` when an agent is
          // selected, `{base}` unchanged otherwise (this client never sends
          // a `conv` nonce, so that half of the formula never applies).
          // Mirrored here — an undocumented but stable internal format, not
          // part of the public wire contract — purely so a LATER connection
          // can address the same bucket to resume it.
          const computed = this.currentAgent
            ? `${this.connectionBaseSid}#agent:${this.currentAgent}`
            : this.connectionBaseSid;
          this.activeSid = computed;
          this.persistSid(computed);
        }
      }

      const frame =
        m.type === 'error' && typeof m.message === 'string'
          ? { ...m, message: friendlyErrorMessage(m.message) }
          : m;
      this.post({ type: 'chat', frame });
    });
    sock.on('close', () => {
      if (this.chat === sock) this.chat = undefined;
      this.post({ type: 'chat-closed' });
    });
    sock.on('error', () => { /* close follows */ });
    await new Promise<void>((resolve, reject) => {
      sock.once('open', () => resolve());
      sock.once('error', (e: Error) => reject(e));
    });
    sock.send(JSON.stringify({ type: 'auth', token }));
    await Promise.race([
      connectFrameSeen,
      new Promise<void>((resolve) => setTimeout(resolve, 3000)),
    ]);
    return sock;
  }

  /** Persists the most recently known session id for `currentAgent` under
   * (gatewayUrl, agent) — see `sessionStateKey` in config.ts. Fire-and-forget:
   * `Memento.update` essentially never rejects in practice, and a lost write
   * only costs a future resume attempt, never correctness. */
  private persistSid(sid: string): void {
    void this.ctx.workspaceState.update(sessionStateKey(this.currentAgent), sid);
  }

  private loadPersistedSid(): string | undefined {
    return this.ctx.workspaceState.get<string>(sessionStateKey(this.currentAgent));
  }

  private clearPersistedSid(): void {
    void this.ctx.workspaceState.update(sessionStateKey(this.currentAgent), undefined);
  }

  async sendChat(text: string): Promise<void> {
    const sock = await this.ensureChat();
    let sid: string | undefined = this.activeSid;
    if (!sid) {
      // No bucket established yet on THIS connection — try a persisted id
      // from a previous connection (design doc §4 P1 "重連時帶上以續聊").
      // Absent ⇒ send no `session_id` at all so the gateway takes the plain
      // compose-a-new-bucket path; sending a self-guessed id here (before it
      // has ever been created) would instead trigger the RESUME lookup for a
      // bucket that doesn't exist and fail with "conversation not found".
      const persisted = this.loadPersistedSid();
      if (persisted) {
        sid = persisted;
        this.activeSid = persisted;
      }
    }
    this.resumeAttempted = Boolean(sid);
    sock.send(
      JSON.stringify({
        type: 'user_message',
        content: text,
        // `agent` — WebChat protocol field (`ChatMessage::UserMessage.agent`
        // in `crates/duduclaw-gateway/src/webchat.rs`). Absent when no agent
        // has been selected yet ⇒ server default/main-agent path.
        ...(this.currentAgent ? { agent: this.currentAgent } : {}),
        ...(sid ? { session_id: sid } : {}),
      })
    );
  }

  // ── dashboard JSON-RPC ──
  private async ensureRpc(retried = false): Promise<void> {
    if (this.rpcSock && this.rpcSock.readyState === WebSocket.OPEN && this.rpcReady) {
      return this.rpcReady;
    }
    const token = await this.token();
    if (!token) throw new Error(NOT_AUTHED_ERROR);
    const sock = new WebSocket(this.wsUrl('/ws'));
    this.rpcSock = sock;
    // The gateway speaks its own `WsFrame` protocol (see
    // `crates/duduclaw-gateway/src/protocol.rs`), NOT JSON-RPC 2.0: frames are
    // tagged `{"type":"req"|"res"|"event"}`, a reply carries `ok` + `payload`
    // (not `result`), and `error` is a bare JSON value — usually a string.
    // Server-pushed `event` frames have no `id` and must be ignored here.
    sock.on('message', (raw: Buffer) => {
      let f: {
        type?: string;
        id?: unknown;
        ok?: boolean;
        payload?: unknown;
        error?: unknown;
      };
      try { f = JSON.parse(raw.toString('utf8')); } catch { return; }
      if (f.type !== 'res' || f.id == null) return;
      const p = this.pending.get(String(f.id));
      if (!p) return;
      this.pending.delete(String(f.id));
      if (f.ok === false) {
        const msg = typeof f.error === 'string' ? f.error : JSON.stringify(f.error);
        p.reject(new Error(msg || 'request failed'));
      } else {
        p.resolve(f.payload);
      }
    });
    sock.on('close', () => {
      if (this.rpcSock === sock) { this.rpcSock = undefined; this.rpcReady = undefined; }
      for (const [, p] of this.pending) p.reject(new Error('connection closed'));
      this.pending.clear();
    });
    sock.on('error', () => { /* close follows */ });
    await new Promise<void>((resolve, reject) => {
      sock.once('open', () => resolve());
      sock.once('error', (e: Error) => reject(e));
    });
    this.rpcReady = this.rawRpc('connect', { jwt: token }).then(
      () => undefined,
      async (e: Error) => {
        if (retried) throw e;
        const t = await refreshAccessToken(this.ctx);
        sock.close();
        if (!t) { this.post({ type: 'auth-required' }); throw e; }
        return this.ensureRpc(true);
      }
    );
    return this.rpcReady;
  }

  private rawRpc(method: string, params: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = String(++this.rpcSeq);
      this.pending.set(id, { resolve, reject });
      // `type: 'req'` is required — an untagged frame fails the gateway's
      // `WsFrame` deserialization, which it treats as a failed handshake and
      // closes the socket ("WebSocket auth failed"), surfacing here as the
      // misleading 'connection closed'.
      this.rpcSock?.send(JSON.stringify({ type: 'req', id, method, params }));
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${method} timeout`));
      }, 15000);
    });
  }

  async rpc(method: string, params: unknown): Promise<unknown> {
    await this.ensureRpc();
    return this.rawRpc(method, params);
  }

  /**
   * `agents.list` — server-side already filtered by `ctx.visible_agents()`
   * (binding-based for non-admins; unfiltered for admins). Returns `{agents:
   * [...]}` (see `AgentListEntry` in types.ts); this just unwraps it.
   */
  async agentsList(): Promise<AgentListEntry[]> {
    const res = (await this.rpc('agents.list', {})) as { agents?: AgentListEntry[] } | undefined;
    return res?.agents ?? [];
  }

  /**
   * `tasks.list` — `handle_tasks_list` in `crates/duduclaw-gateway/src/handlers.rs`
   * (~line 27784), gated by `check_agent_filter!(AccessLevel::Viewer)`: a
   * non-admin caller MUST scope the query to a bound `agent_id` or the RPC
   * rejects with "agent_id parameter is required" — this client always has
   * one (the selected AI employee) so it is always passed. Returns
   * `{tasks: [...]}`; this just unwraps it.
   */
  async tasksList(agentId: string): Promise<TaskItem[]> {
    const res = (await this.rpc('tasks.list', { agent_id: agentId })) as
      | { tasks?: TaskItem[] }
      | undefined;
    return res?.tasks ?? [];
  }

  /**
   * `tasks.goal_create` — `handle_tasks_goal_create` in
   * `crates/duduclaw-gateway/src/handlers.rs` (~line 28565). Requires
   * `AccessLevel::Operator` on `agentId` (`acl::require_agent_access`) —
   * an insufficiently-bound caller gets the literal `"permission denied"`
   * error, which `friendlyErrorMessage` (auth.ts) already renders as
   * "此功能需要管理者權限。" through the same `rpc()` error path every other
   * RPC call uses.
   */
  async goalCreate(agentId: string, description: string, planFirst: boolean): Promise<GoalCreateResult> {
    return (await this.rpc('tasks.goal_create', {
      agent_id: agentId,
      description,
      plan_first: planFirst,
    })) as GoalCreateResult;
  }

  /**
   * `tasks.goal_decide` — `handle_tasks_goal_decide` in
   * `crates/duduclaw-gateway/src/handlers.rs` (~line 28873). Mirrors the
   * dashboard's shared `NeedsHumanActions` component
   * (`web/src/components/inbox/NeedsHumanTaskPanel.tsx`): the same three
   * actions (`retry`/`done`/`abort`), `note` optional and only meaningful
   * for `retry` (server defaults it to `""` either way). Requires
   * `AccessLevel::Operator` via `authorize_task_access`, same permission
   * shape as `goalCreate` above.
   */
  async goalDecide(taskId: string, action: 'retry' | 'done' | 'abort', note?: string): Promise<unknown> {
    return this.rpc('tasks.goal_decide', { task_id: taskId, action, note: note ?? '' });
  }
}
