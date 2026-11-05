import { create } from 'zustand';
import { parseHistoryAttachments, type HistoryAttachment } from '@/lib/history-attachments';
import { useAuthStore } from './auth-store';
import { useSystemStore } from './system-store';
import type { VisemeShape } from '@/components/mascot';
import { REST_VISEME, sampleViseme } from '@/components/chat/viseme-sampler';
import { loadTtsEnabled, saveTtsEnabled } from '@/components/chat/tts-playback';
import { effectiveName } from '@/lib/product-identity';
import type { ChatArtifact } from '@/components/console/artifact-types';
import { lookupMessage, toast } from '@/lib/toast';
import { isOtherConversationFailure, isTurnActivity } from './chat-silence';
import { mentionErrorText } from '@/lib/mention';
import { attachHistoryCitations, parseChatCitations, type ChatCitation, type SessionCitationRow } from '@/lib/knowledge-citations';
import {
  applyThinking,
  finishTurnThinking,
  parseThinkingDoneMeta,
  startTurnThinking,
  type ThinkingFrame,
  type TurnThinking,
} from './chat-thinking';

import { IN_TURN_FRAME_TYPES, parseChatArtifact, parseComputerWatch, shouldRenderFrame, type ComputerWatchRef } from './chat-frames';

export * from './chat-thinking';
export * from './chat-frames';

export interface ResolvedChatAgent {
  readonly id: string;
  readonly name: string;
  readonly icon: string;
}
export interface ChatAssignmentError {
  readonly code: string;
  readonly message: string;
  readonly reason?: string;
}

/** Personal and legacy admin connections retain their existing partner selection. */
export function requiresAssignedChatAgent(): boolean {
  const user = useAuthStore.getState().user;
  return user !== null && user.role !== 'admin'
    && useSystemStore.getState().status?.edition_profile !== 'personal';
}

export function chatAccessMessageKey(code: string, reason?: string): string | null {
  if (code === 'agent_forbidden') return 'webchat.assignment.forbidden';
  if (code === 'main_agent_admin_only') return 'webchat.assignment.mainAdminOnly';
  if (code === 'no_assigned_agent' && reason === 'unavailable') return 'webchat.assignment.unavailable';
  if (code === 'no_assigned_agent') return reason === 'multiple_agents'
    ? 'webchat.assignment.multiple' : 'webchat.assignment.none';
  return null;
}

export interface ChatAttachmentMeta {
  readonly name: string;
  readonly mime: string;
}

export interface ChatMessage {
  readonly id: string;
  readonly role: 'user' | 'assistant' | 'system';
  readonly content: string;
  readonly timestamp: number;
  readonly turnId?: string;
  readonly delivery?: 'queued' | 'running' | 'done' | 'cancelled' | 'failed';
  readonly deliveryError?: string;
  readonly liveActivity?: TurnActivity;
  readonly tokens?: number;
  /** Pending uploads use chips; server-issued history references use file cards. */
  readonly attachments?: readonly (ChatAttachmentMeta | HistoryAttachment)[];
  /**
   * A structured result/confirmation card (O-3) an assistant reply carries
   * alongside its plain text — see `@/components/console/artifact-types.ts`
   * for the full contract. Populated from `assistant_done`'s optional
   * `artifact` field (validated by `parseChatArtifact` in `chat-frames.ts`) when the
   * backend's O-4 pending-op marker maps to one (`confirm_action` for a
   * pending restart/shutdown/factory-reset, `update_confirm` for a pending
   * `os_apply_update` — see `os_operator::marker_to_artifact` on the
   * gateway). Also reconstructed by `historyToMessages` (resumed-conversation
   * history): the gateway persists the raw marker into session storage and
   * replays the same strip+map on `chat.sessions.history` reads
   * (`handlers::chat_history_row_content_and_artifact`), so a resumed
   * conversation's earlier confirmation card renders again instead of a bare
   * marker tag or a silently-missing card.
   */
  readonly artifact?: ChatArtifact;
  /** Knowledge-base sources cited by `[n]` markers in this reply (validated). */
  readonly citations?: readonly ChatCitation[];
  /** Stored row id from `turn_accepted` or `chat.sessions.history`. */
  readonly historyId?: number;
  /** The AI staff member that answered (from `assistant_done.agent`). Differs
   *  from the selected partner when the message called someone else by `@`. */
  readonly agentId?: string;
  /** That staff member's display name (`assistant_done.agent_name`). */
  readonly agentName?: string;
  /** A task this reply created (`/交辦`, from `assistant_done.task`). */
  readonly task?: AssignedTaskRef;
  readonly computerWatch?: ComputerWatchRef;
  /** The turn's thinking (wall time, tokens, masked blocks) — in memory only,
   *  never part of the stored history. */
  readonly thinking?: TurnThinking;
}

/** A task created from the chat, as `assistant_done.task` carries it. */
export interface AssignedTaskRef {
  readonly id: string;
  readonly agent: string;
}

/** Validate `assistant_done.task`: both fields non-empty strings, else
 *  `undefined` (an older or hand-crafted frame shows no card). Pure. */
export function parseAssignedTask(raw: unknown): AssignedTaskRef | undefined {
  if (raw === null || typeof raw !== 'object') return undefined;
  const { id, agent } = raw as { id?: unknown; agent?: unknown };
  if (typeof id !== 'string' || !id || typeof agent !== 'string' || !agent) return undefined;
  return { id, agent };
}

/** A file selected for upload, already read into base64. */
export interface PendingAttachment {
  readonly name: string;
  readonly mime: string;
  readonly dataBase64: string;
}

/** One live "agentic task insight" step (task-board update) streamed while the
 *  agent works, from the gateway `progress` messages. Tool activity now flows
 *  through the structured `step` frame / `StepNode` tree instead. */
export interface ChatStep {
  readonly id: string;
  /** "tool" | "todo". */
  readonly kind: string;
  /** Tool name for `kind === 'tool'`. */
  readonly tool?: string;
  readonly detail?: string;
  readonly content: string;
  readonly ts: number;
}

/** A node in the live tool step tree (V7 / T7.3), folded from the gateway's
 *  structured `step` frames (`{type:"step",phase,tool,summary?,depth,ts}`).
 *  `start` opens a node (running); the matching `end` marks it done. `depth` is
 *  the indentation level (a `Task` sub-agent's inner tools nest at depth ≥ 1). */
export interface StepNode {
  readonly id: string;
  readonly tool: string;
  readonly summary?: string;
  readonly depth: number;
  /** True while the call is in flight (spinner); false once its `end` lands. */
  readonly running: boolean;
  readonly ts: number;
}

/** The wire shape of a `step` frame. */
export interface StepFrame {
  readonly phase: string;
  readonly tool: string;
  readonly summary?: string;
  readonly depth?: number;
  readonly ts?: number;
}

/**
 * Fold one `step` frame into the tool step tree (pure — exported for tests).
 *
 * Fault tolerance (the wire is best-effort, so the reducer never throws):
 *  - `phase:"start"` → append a running node at `depth` (default 0).
 *  - `phase:"end"`   → mark the most-recent still-running node with the same
 *    `tool` as done. An `end` with no matching open node is ignored (a
 *    duplicate/orphan end never corrupts the tree).
 *  - a start that never gets its `end` simply stays `running` (spinner) until
 *    the turn is reset — an honest "still working" signal, not a hang.
 *  - any other `phase` value is ignored.
 */
export function applyStep(tree: readonly StepNode[], frame: StepFrame): readonly StepNode[] {
  if (frame.phase === 'start') {
    return [
      ...tree,
      {
        id: nextId(),
        tool: frame.tool,
        summary: frame.summary,
        depth: frame.depth ?? 0,
        running: true,
        ts: frame.ts ?? Date.now(),
      },
    ];
  }
  if (frame.phase === 'end') {
    // Close the last open node with this tool name.
    for (let i = tree.length - 1; i >= 0; i -= 1) {
      if (tree[i].running && tree[i].tool === frame.tool) {
        const next = [...tree];
        next[i] = { ...next[i], running: false };
        return next;
      }
    }
    return tree; // orphan end — ignore
  }
  return tree; // unknown phase — ignore
}

/**
 * Which session id survives a fresh `session_info` hello (pure — exported for
 * tests). A conversation the user explicitly resumed wins over the connection's
 * own id: resuming from the sidebar 對話紀錄 happens BEFORE the chat socket
 * exists, so adopting the hello id there would silently start a new thread on
 * the next send while the resumed transcript is still on screen.
 */
export function sessionIdAfterHello(
  current: string | null,
  resumedExplicitly: boolean,
  helloSessionId: string,
): string | null {
  return resumedExplicitly && current ? current : helloSessionId;
}

/** Where the assistant is in the current turn — drives DuDu's face (V7/T7.1). */
export type ChatPhase = 'idle' | 'thinking' | 'speaking' | 'done' | 'error';

interface ChatStore {
  readonly messages: readonly ChatMessage[];
  /** Live task-board insight steps for the running turn (preserved when queueing). */
  readonly steps: readonly ChatStep[];
  /** Live tool step tree for the current turn (folded from `step` frames). */
  readonly stepTree: readonly StepNode[];
  /** The running turn's thinking, until its reply lands (then it
   *  moves onto that assistant message). Null between turns. */
  readonly thinking: TurnThinking | null;
  readonly isStreaming: boolean;
  /** Client message id of the foreground running turn; queued turns keep theirs on the user message. */
  readonly activeMessageId: string | null;
  readonly lastCompletedReplyId: string | null;
  readonly lastTurnFrameAt: number | null;
  readonly historyReplyRevision: number;
  readonly historyReconnectRevision: number;
  /** Fine-grained turn phase for the companion face (thinking/speaking/…). */
  readonly phase: ChatPhase;
  /** Current mouth shape while `speaking` — sampled per assistant chunk. */
  readonly viseme: VisemeShape;
  readonly sessionId: string | null;
  /** Canonical session supplied by a correlated accepted turn, including @ routing. */
  readonly acceptedSessionId: string | null;
  /**
   * The session id the server assigned to THIS connection, captured from the
   * first `session_info` frame. Held in store state (not a module variable) so
   * it's cleared on reconnect, visible in devtools, and assertable in tests. It
   * is the "home base" the active `sessionId` is restored to whenever the user
   * leaves a resumed historical conversation — a new conversation (`reset`),
   * switching partner (`selectAgent`), or a resume miss. Without this, a
   * resumed `sessionId` would leak into the next `/new` (archiving the wrong
   * session) or the next send (read by the server as a cross-agent resume).
   */
  readonly ownSessionId: string | null;
  /**
   * True while the view shows a conversation the user explicitly resumed, so the
   * first `session_info` after a connect must NOT overwrite `sessionId` with the
   * connection's own id. Resuming from the sidebar 對話紀錄 happens BEFORE the
   * chat socket exists — without this flag the follow-up send would silently
   * open a new thread instead of continuing the conversation on screen. Cleared
   * by `reset` / `selectAgent`, which both leave the resumed conversation.
   */
  readonly resumedExplicitly: boolean;
  /**
   * The conversation nonce for the conversation currently open (bumped on every
   * `/new`, partner switch, and resume). Sent on each `user_message` as `conv`;
   * the gateway (a) scopes the turn to its own server-side session bucket
   * (`…#conv:<nonce>`) and (b) echoes it on every reply frame. The socket loop
   * drops any reply frame whose `conv` differs from this — so a long task started
   * in conversation A finishes into A's history, not into a conversation B the
   * user opened while it was still running.
   */
  readonly convId: string;
  /**
   * Monotonic counter bumped once per completed reply (`assistant_done`) for ANY
   * conversation — including one whose frames were dropped by the attribution
   * guard. The WebChat page watches it to refresh the past-conversation list, so
   * a brand-new conversation (and its title) shows up the moment its first reply
   * lands, instead of only after a manual reload. Without this, opening a new
   * conversation B, letting it finish, then returning to A left B unreachable
   * (its `…#conv:<nonce>` session existed server-side but never appeared in the
   * list to be resumed).
   */
  readonly sessionsRevision: number;
  readonly agentName: string;
  readonly agentIcon: string;
  /** The AI staff member currently chosen as the conversation partner, or null
   *  for the default office assistant (DuDu). Visual selection (T7.2); the
   *  backend socket still routes to the main agent. */
  readonly selectedAgentId: string | null;
  /** Undefined until the server announces the authenticated user's assignment. */
  readonly resolvedAgent: ResolvedChatAgent | null | undefined;
  readonly assignmentError: ChatAssignmentError | null;
  /** Whether the agent's model can interpret uploaded images (from session_info). */
  readonly supportsVision: boolean;
  /** The agent's preferred model id (from session_info). */
  readonly model: string;
  readonly connectionState: 'disconnected' | 'connecting' | 'connected';

  // ── Voice (openhuman-parity B) ──────────────────────────────
  /** True while the mic is actively capturing a push-to-talk clip. Drives the
   *  DuDu `listening` face. */
  readonly isRecording: boolean;
  /** True while a captured clip is uploading / being transcribed. */
  readonly isTranscribing: boolean;
  /** Reply-playback toggle (persisted to `localStorage`). When on, completed
   *  assistant replies are spoken via `POST /api/tts`. */
  readonly ttsEnabled: boolean;

  connect: () => void;
  disconnect: () => void;
  send: (text: string, attachments?: readonly PendingAttachment[]) => void;
  cancel: (msgId?: string) => void;
  reset: () => void;
  /** Pick the conversation partner (null → DuDu). Visual only for now. */
  selectAgent: (id: string | null) => void;
  /**
   * Resume a past conversation (WP3). Renders the loaded history and points the
   * active session at `sessionId` so the NEXT `user_message` frame carries it —
   * the gateway then resumes that server-side session and confirms with a
   * `session_info` frame. The current partner selection is left untouched (the
   * history list is already scoped to that partner, so the session's owner
   * matches it).
   */
  resumeSession: (sessionId: string, messages: readonly ChatMessage[]) => void;
  /** Detach displayed history without sending a reset/archive to that id. */
  discardHistory: (identityChanged?: boolean) => void;
  /** Attach knowledge citations (by stored row id) to the resumed session's
   *  assistant messages. No-op when the active session changed meanwhile. */
  applyHistoryCitations: (sessionId: string, rows: readonly SessionCitationRow[]) => void;
  setRecording: (v: boolean) => void;
  setTranscribing: (v: boolean) => void;
  setTtsEnabled: (v: boolean) => void;
}

type TurnActivity = Pick<ChatStore, 'thinking' | 'steps' | 'stepTree' | 'phase' | 'viseme' | 'lastTurnFrameAt'>;

function currentActivity(state: ChatStore): TurnActivity {
  const { thinking, steps, stepTree, phase, viseme, lastTurnFrameAt } = state;
  return { thinking, steps, stepTree, phase, viseme, lastTurnFrameAt };
}

function freshActivity(): TurnActivity {
  return { thinking: startTurnThinking(Date.now()), steps: [], stepTree: [],
    phase: 'thinking', viseme: REST_VISEME, lastTurnFrameAt: Date.now() };
}

/** Different @ routes can have independent server lanes in the same view. */
function remainingActivity(messages: readonly ChatMessage[], phase: 'done' | 'error') {
  const running = messages.find(m => m.role === 'user' && m.delivery === 'running');
  return {
    ...(running?.liveActivity ?? { thinking: null, phase, viseme: REST_VISEME }),
    activeMessageId: running?.turnId ?? null,
    isStreaming: !!running || messages.some(m => m.delivery === 'queued'),
  };
}

let msgCounter = 0;

function nextId(): string {
  msgCounter += 1;
  return `msg-${Date.now()}-${msgCounter}`;
}

let convCounter = 0;

/** Mint a fresh conversation nonce. Kept short and `[A-Za-z0-9_-]`-only so it
 *  survives the gateway's `sanitize_conv_nonce` unchanged. */
function nextConvId(): string {
  convCounter += 1;
  return `c-${Date.now().toString(36)}-${convCounter}`;
}

let clientMsgCounter = 0;

/** A fresh client message id (`[A-Za-z0-9_-]`, at most 64 characters). */
export function newClientMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;
  clientMsgCounter += 1;
  return `m-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}-${clientMsgCounter}`;
}

/**
 * Assemble the `user_message` wire frame (pure — exported for tests).
 *
 * L1: `agent` is included only when a partner is selected; for the default
 * assistant the key is omitted entirely so the wire stays byte-compatible with
 * the pre-L1 protocol (the gateway treats absent === default agent).
 *
 * `conv` is the conversation nonce (see `ChatStore.convId`) — included when set
 * so the gateway scopes the turn to its own session bucket and echoes it back on
 * every reply frame for attribution. Omitted when null (legacy single-bucket).
 */
export function buildUserMessageFrame(opts: {
  content: string;
  sessionId: string | null;
  agentId: string | null;
  attachments: readonly PendingAttachment[];
  convId?: string | null;
  /** One id per send: a `/交辦` derives its task id from it on the server. */
  msgId?: string | null;
}): Record<string, unknown> {
  const { content, sessionId, agentId, attachments, convId, msgId } = opts;
  return {
    type: 'user_message',
    content,
    session_id: sessionId,
    ...(agentId ? { agent: agentId } : {}),
    ...(convId ? { conv: convId } : {}),
    ...(msgId ? { msg_id: msgId } : {}),
    attachments: attachments.map((a) => ({
      filename: a.name,
      mime: a.mime,
      data_base64: a.dataBase64,
    })),
  };
}

/** Correlate additive turn ids without changing legacy connection frames.
 * `conv` identifies the view even when an @ call routes to another session.
 * `msg_id` rejects late frames from an earlier turn in the same view. A frame
 * with only a session id must match the resumed or composed current session.
 */
export function shouldRenderChatFrame(
  frame: Record<string, unknown>,
  current: Pick<ChatStore, 'convId' | 'activeMessageId' | 'sessionId' | 'selectedAgentId' | 'resumedExplicitly'> & Partial<Pick<ChatStore, 'messages'>>,
): boolean {
  // Control-request failures never complete the targeted turn. Older gateways
  // echoed its msg_id here, so do not infer terminal ownership from that field.
  if (frame.type === 'error' && (frame.code === 'turn_not_found' || frame.code === 'duplicate_message')) {
    return false;
  }
  const conv = typeof frame.conv === 'string' && frame.conv ? frame.conv : null;
  const requestId = typeof frame.request_id === 'string' && frame.request_id ? frame.request_id : null;
  const msg = frame.type === 'error' && (frame.code === 'turn_limit' || frame.code === 'identity_unavailable')
    ? requestId ?? (typeof frame.msg_id === 'string' && frame.msg_id ? frame.msg_id : null)
    : typeof frame.msg_id === 'string' && frame.msg_id ? frame.msg_id : null;
  // An uncorrelated admission refusal must not clear another accepted turn.
  if (frame.type === 'error' && (frame.code === 'turn_limit' || frame.code === 'identity_unavailable') && !msg) return false;
  if (conv && conv !== current.convId) return false;
  if (msg && msg !== current.activeMessageId && !current.messages?.some(m =>
    m.role === 'user' && m.turnId === msg && (m.delivery === 'queued' || m.delivery === 'running'))) return false;
  if (conv || msg) return true;
  if (frame.type !== 'session_info' && typeof frame.session_id === 'string' && frame.session_id) {
    const composed = current.resumedExplicitly
      ? current.sessionId
      : `${current.sessionId}${current.selectedAgentId ? `#agent:${current.selectedAgentId}` : ''}#conv:${current.convId}`;
    return frame.session_id === current.sessionId || frame.session_id === composed;
  }
  return shouldRenderFrame(frame.type, frame.conv, current.convId);
}

/** Insert a reply beside its user message, even with later queued turns. */
function replySlot(messages: readonly ChatMessage[], turnId: string | null): number {
  const user = messages.findIndex(m => m.role === 'user' && m.turnId === turnId);
  if (user < 0) return messages.length - (messages.at(-1)?.role === 'assistant' ? 1 : 0);
  return user + 1;
}

function finishMessage(messages: readonly ChatMessage[], turnId: string | null, delivery: 'done' | 'failed' | 'cancelled') {
  return messages.map(m => m.role === 'user' && m.turnId === turnId ? { ...m, delivery, liveActivity: undefined } : m);
}

/** The wire shape of one message returned by `chat.sessions.history`. */
export interface HistoryMessageWire {
  readonly attachments?: unknown;
  readonly notice_kind?: string | null;
  readonly role: string;
  readonly content: string;
  /** RFC3339 timestamp. */
  readonly timestamp: string;
  readonly tokens?: number;
  /**
   * O-4→O-3 resume wiring: present only on an assistant row whose stored
   * text carried an O-4 pending-op marker that mapped to an O-3 artifact
   * (see `handlers::chat_history_row_content_and_artifact` on the gateway —
   * `chat_history_row_content_and_artifact_tests` covers the strip+map).
   * Untyped on the wire like `assistant_done`'s `artifact` field; validated
   * below by the same `parseChatArtifact` guard before it ever reaches
   * `ChatMessage` state.
   */
  readonly artifact?: unknown;
  /** Stored message row id (optional; newer gateways). */
  readonly id?: number;
  /** Original user-message id, persisted even if its acknowledgement is lost. */
  readonly client_msg_id?: string | null;
}

/**
 * Map history-RPC messages into the store's `ChatMessage` shape (pure — exported
 * for tests). Unknown roles collapse to `user`; an unparseable timestamp falls
 * back to "now" so a bad row never breaks the render. A malformed/unknown
 * `artifact` (older gateway, hand-crafted frame) degrades to "no card" via
 * `parseChatArtifact`, never a broken render.
 */
export function historyToMessages(raw: readonly HistoryMessageWire[]): ChatMessage[] {
  return raw.map((m) => {
    const role: ChatMessage['role'] =
      m.notice_kind === 'turn_failure' ? 'system' : m.role === 'assistant' ? 'assistant' : m.role === 'system' ? 'system' : 'user';
    const ts = Date.parse(m.timestamp);
    const artifact = role === 'assistant' ? parseChatArtifact(m.artifact) : undefined;
    const attachments = parseHistoryAttachments(m.attachments);
    return {
      id: nextId(),
      role,
      content: m.content ?? '',
      timestamp: Number.isFinite(ts) ? ts : Date.now(),
      ...(typeof m.tokens === 'number' ? { tokens: m.tokens } : {}),
      ...(artifact ? { artifact } : {}),
      ...(attachments.length ? { attachments } : {}),
      ...(typeof m.id === 'number' && Number.isFinite(m.id) ? { historyId: m.id } : {}),
      ...(m.role === 'user' && typeof m.client_msg_id === 'string' && m.client_msg_id
        ? { turnId: m.client_msg_id } : {}),
    };
  });
}

/** True when a `/ws/chat` error frame reports a resume miss (the session the
 *  client tried to continue no longer exists). Pure — exported for tests. */
export function isResumeNotFound(message: string | null | undefined): boolean {
  return /conversation not found/i.test(String(message ?? ''));
}

// Module-level WebSocket reference — kept outside Zustand to avoid
// serialization issues and enable reconnection logic.
let wsRef: WebSocket | null = null;

// True between "user message sent" and "first server frame for that turn".
// The first frame confirms the server has persisted the user message, so the
// sessions list is refreshed once more (see the bump in `send`).
let awaitingFirstServerFrame = false;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempt = 0;
let intentionalDisconnect = false;
let visibilityListener: (() => void) | null = null;

function subscribeCurrentConversation() {
  if (!wsRef || wsRef.readyState !== WebSocket.OPEN) return;
  const state = useChatStore.getState();
  wsRef.send(JSON.stringify({ type: 'subscribe',
    session_id: state.resumedExplicitly ? state.sessionId : null, conv: state.convId }));
}
const MAX_RECONNECT_ATTEMPTS = 10;

// When the reply stream pauses (no chunk for a beat), let the mouth fall back to
// REST so DuDu isn't frozen mid-vowel. Reset on every chunk; cleared on done.
let visemeIdleTimer: ReturnType<typeof setTimeout> | null = null;
const VISEME_IDLE_MS = 320;

function clearVisemeIdle() {
  if (visemeIdleTimer) {
    clearTimeout(visemeIdleTimer);
    visemeIdleTimer = null;
  }
}

function scheduleReconnect(connectFn: () => void) {
  if (intentionalDisconnect) return;
  if (reconnectAttempt >= MAX_RECONNECT_ATTEMPTS) return;

  const delay = Math.min(1000 * Math.pow(2, reconnectAttempt), 30000);
  reconnectAttempt += 1;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectFn();
  }, delay);
}

export const useChatStore = create<ChatStore>((set, get) => {
  function connect() {
    if (wsRef && (wsRef.readyState === WebSocket.OPEN || wsRef.readyState === WebSocket.CONNECTING)) return;
    if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    if (!visibilityListener) {
      visibilityListener = () => {
        if (document.visibilityState !== 'visible' || intentionalDisconnect) return;
        // A suspended tab may still report OPEN for a dead transport. Replace it
        // immediately, without waiting for a delayed close event or backoff.
        const previous = wsRef;
        wsRef = null;
        previous?.close();
        reconnectAttempt = 0;
        connect();
      };
      document.addEventListener('visibilitychange', visibilityListener);
    }

    intentionalDisconnect = false;
    // Preserve the conversation identity across transport replacement. The
    // connection hello only supplies the base for the next new conversation.
    set(state => {
      const resume = state.sessionId && (state.resumedExplicitly || state.messages.length > 0);
      const sessionId = resume ? state.acceptedSessionId ?? (state.resumedExplicitly ? state.sessionId
        : `${state.sessionId}${state.selectedAgentId ? `#agent:${state.selectedAgentId}` : ''}#conv:${state.convId}`)
        : state.sessionId;
      return { connectionState: 'connecting', ownSessionId: null,
        ...(resume ? { sessionId, resumedExplicitly: true } : { resolvedAgent: undefined, assignmentError: null }),
      };
    });

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${protocol}//${window.location.host}/ws/chat`;
    const socket = new WebSocket(url);
    wsRef = socket;

    socket.onopen = () => {
      if (wsRef !== socket) return;
      reconnectAttempt = 0;
      // C5: the server now requires the first frame to authenticate the
      // connection with the current access token before any message is accepted.
      const jwt = useAuthStore.getState().jwt;
      if (jwt) {
        socket.send(JSON.stringify({ type: 'auth', token: jwt }));
      } else {
        // No session — close; the user must log in first.
        socket.close();
        set({ connectionState: 'disconnected' });
        return;
      }
      set({ connectionState: 'connected' });
    };

    socket.onmessage = (event) => {
      if (wsRef !== socket) return;
      try {
        const data = JSON.parse(event.data);
        // A lost subscription is not a terminal event for an accepted turn.
        // Only a refusal correlated to this exact view may trigger recovery.
        if (data.type === 'error' && data.code === 'subscription_unavailable') {
          if (get().resumedExplicitly && data.session_id === get().sessionId && data.conv === get().convId) {
            set(state => ({ historyReconnectRevision: state.historyReconnectRevision + 1 }));
          }
          return;
        }
        // A finished reply — for ANY conversation, even one whose frames are
        // about to be dropped below — changed the server-side session list
        // (created/updated a session bucket). Signal the page to refresh the
        // list so the conversation becomes resumable right away.
        if (data.type === 'assistant_done' || (data.type === 'error' && data.code === 'cancelled')) {
          set((state) => ({ sessionsRevision: state.sessionsRevision + 1 }));
        }
        // Errors and resume confirmations can now belong to a turn too.
        // Reject another turn before changing the foreground activity state.
        if (data.type === 'assistant_done' && data.session_id === get().sessionId) {
          set(state => ({ historyReplyRevision: state.historyReplyRevision + 1 }));
        }
        if (!shouldRenderChatFrame(data, get())) {
          if (isOtherConversationFailure(data, get())) toast.info(lookupMessage('chat.otherConversationFailed'));
          return;
        }
        if (data.type !== 'error' && typeof data.session_id === 'string' && data.session_id
          && data.conv === get().convId && typeof data.msg_id === 'string'
          && get().messages.findLast(message => message.role === 'user')?.turnId === data.msg_id) {
          set({ acceptedSessionId: data.session_id });
        }
        const frameTurnId = data.type === 'error' && ['turn_limit', 'identity_unavailable'].includes(data.code)
          ? data.request_id ?? data.msg_id : data.msg_id;
        const targetTurnId = typeof frameTurnId === 'string' ? frameTurnId : get().activeMessageId;
        if ((IN_TURN_FRAME_TYPES.has(data.type) || data.type === 'assistant_done') && targetTurnId) {
          const user = get().messages.find(m => m.role === 'user' && m.turnId === targetTurnId);
          if (user && targetTurnId !== get().activeMessageId) {
            clearVisemeIdle();
            set(state => ({
              messages: state.messages.map(m => m === user ? { ...m, delivery: 'running' as const }
                : m.role === 'user' && m.turnId === state.activeMessageId
                  ? { ...m, liveActivity: currentActivity(state) } : m),
              activeMessageId: targetTurnId, isStreaming: true,
              ...(user.liveActivity ?? freshActivity()),
            }));
          }
        }
        if (get().isStreaming && isTurnActivity(data, get().activeMessageId)) {
          set({ lastTurnFrameAt: Date.now() });
        }
        if (awaitingFirstServerFrame && IN_TURN_FRAME_TYPES.has(data.type)) {
          awaitingFirstServerFrame = false;
          set((state) => ({ sessionsRevision: state.sessionsRevision + 1 }));
        }
        switch (data.type) {
          case 'turn_accepted': {
            if (typeof data.msg_id === 'string' && Number.isSafeInteger(data.user_row_id) && data.user_row_id > 0) {
              set(state => ({ messages: state.messages.map(message =>
                message.role === 'user' && message.turnId === data.msg_id
                  ? { ...message, historyId: data.user_row_id } : message) }));
            }
            break;
          }
          case 'subscribed': {
            if (data.session_id === get().sessionId && data.conv === get().convId) {
              set(state => ({ historyReconnectRevision: state.historyReconnectRevision + 1 }));
            }
            break;
          }
          case 'session_info': {
            // A connection hello describes the default partner, not a history
            // session opened before the socket connected.
            const preserveResumedPartner = requiresAssignedChatAgent() && get().resumedExplicitly
              && data.session_id !== get().sessionId;
            const metadata = preserveResumedPartner ? {} : {
              agentName: data.agent_name,
              agentIcon: data.agent_icon,
              supportsVision: data.supports_vision ?? false,
              model: data.model ?? '',
            };
            if ('resolved_agent' in data && !preserveResumedPartner) {
              const candidate = data.resolved_agent;
              const resolvedAgent = candidate && typeof candidate.id === 'string'
                && typeof candidate.name === 'string' && typeof candidate.icon === 'string'
                ? candidate as ResolvedChatAgent : null;
              set({ resolvedAgent, assignmentError: data.assignment_error ?? null,
                selectedAgentId: resolvedAgent?.id ?? null });
            }
            // Both configuration refreshes and per-message assignment metadata
            // leave the base session id intact. Only resume confirmations adopt
            // a persisted session id.
            if (data.refresh) {
              set(metadata);
              break;
            }
            const hello = get().ownSessionId === null;
            set((state) => ({
              ownSessionId:
                state.ownSessionId === null && typeof data.session_id === 'string'
                  ? data.session_id
                  : state.ownSessionId,
              sessionId: sessionIdAfterHello(
                state.sessionId,
                state.resumedExplicitly,
                data.session_id,
              ),
              ...metadata,
            }));
            if (hello && get().resumedExplicitly) subscribeCurrentConversation();
            break;
          }

          case 'assistant_chunk':
            // Streaming chunk — append beside the matching user message.
            // Sample the mouth shape off the chunk cadence (T7.1) and arm the
            // idle timer so a stream pause relaxes the mouth back to REST.
            set((state) => {
              const msgs = [...state.messages];
              const slot = replySlot(msgs, targetTurnId);
              const last = msgs[slot];
              if (last && last.role === 'assistant' && !('tokens' in last)) {
                msgs[slot] = {
                  ...last,
                  content: last.content + data.content,
                };
              } else {
                msgs.splice(slot, 0, {
                  id: nextId(),
                  turnId: targetTurnId ?? undefined,
                  role: 'assistant',
                  content: data.content,
                  timestamp: Date.now(),
                });
              }
              return {
                messages: msgs,
                phase: 'speaking' as ChatPhase,
                viseme: sampleViseme(state.viseme, String(data.content ?? '')),
              };
            });
            clearVisemeIdle();
            visemeIdleTimer = setTimeout(() => {
              visemeIdleTimer = null;
              set({ viseme: REST_VISEME });
            }, VISEME_IDLE_MS);
            break;

          case 'step':
            // Structured tool-step boundary (T7.3). Fold into the step tree;
            // pairing / orphan handling lives in the pure `applyStep` reducer.
            set((state) => ({
              stepTree: applyStep(state.stepTree, {
                phase: data.phase,
                tool: data.tool,
                summary: data.summary,
                depth: data.depth,
                ts: data.ts,
              }),
            }));
            break;

          case 'thinking':
            // Already masked and truncated by the gateway; `applyThinking`
            // validates every field. Only folded while a turn is open — a
            // straggler after the reply has nowhere to go.
            set((state) =>
              state.thinking ? { thinking: applyThinking(state.thinking, data as ThinkingFrame) } : {},
            );
            break;

          case 'progress': {
            // Live task-board insights. Keepalives are just "still working"
            // heartbeats. Tool activity now arrives via the richer `step` frame,
            // so we only keep non-tool (task-board `todo`) progress here to avoid
            // double-listing every tool call.
            if (data.kind === 'keepalive') break;
            if (data.kind === 'tool') break;
            set((state) => ({
              steps: [
                ...state.steps,
                {
                  id: nextId(),
                  kind: data.kind ?? 'todo',
                  tool: data.tool,
                  detail: data.detail,
                  content: data.content ?? '',
                  ts: Date.now(),
                },
              ],
            }));
            break;
          }

          case 'computer_watch': {
            const computerWatch = parseComputerWatch(data);
            if (!computerWatch || !get().isStreaming || data.conv !== get().convId) break;
            set((state) => {
              const messages = [...state.messages];
              const slot = replySlot(messages, targetTurnId);
              const last = messages[slot];
              if (last?.role === 'assistant') {
                messages[slot] = { ...last, computerWatch };
              } else {
                messages.splice(slot, 0, { id: nextId(), turnId: targetTurnId ?? undefined, role: 'assistant', content: '',
                  timestamp: Date.now(), computerWatch });
              }
              return { messages };
            });
            break;
          }

          case 'assistant_done': {
            // O-4→O-3: validate the optional wire artifact once per frame —
            // `undefined` for the overwhelming majority of replies (anything
            // that never went through O-4's pending-op path on the backend).
            const artifact = parseChatArtifact(data.artifact);
            const citations = parseChatCitations(data.citations);
            const answeredBy = {
              ...(typeof data.agent === 'string' && data.agent ? { agentId: data.agent } : {}),
              ...(typeof data.agent_name === 'string' && data.agent_name ? { agentName: data.agent_name } : {}),
            };
            const thinkingMeta = parseThinkingDoneMeta(data.thinking);
            const assignedTask = parseAssignedTask(data.task);
            const taskField = assignedTask ? { task: assignedTask } : {};
            set((state) => {
              const msgs = finishMessage(state.messages, targetTurnId, 'done');
              const slot = replySlot(msgs, targetTurnId);
              const last = msgs[slot];
              // The turn's thinking moves onto its reply; the wall time ends now.
              const thinking = state.thinking
                ? { thinking: finishTurnThinking(state.thinking, Date.now(), thinkingMeta) }
                : {};
              if (last && last.role === 'assistant') {
                // Update with full content + tokens
                msgs[slot] = {
                  ...last,
                  content: data.content,
                  tokens: data.tokens_used,
                  ...(artifact ? { artifact } : {}),
                  ...(citations ? { citations } : {}),
                  ...answeredBy,
                  ...thinking,
                  ...taskField,
                };
              } else {
                msgs.splice(slot, 0, {
                  id: nextId(),
                  turnId: targetTurnId ?? undefined,
                  role: 'assistant',
                  content: data.content,
                  timestamp: Date.now(),
                  tokens: data.tokens_used,
                  ...(artifact ? { artifact } : {}),
                  ...(citations ? { citations } : {}),
                  ...answeredBy,
                  ...thinking,
                  ...taskField,
                });
              }
              return {
                messages: msgs,
                lastCompletedReplyId: msgs[slot].id,
                ...remainingActivity(msgs, 'done'),
                // The backend reported the model that ACTUALLY produced this
                // reply (post CLI-side substitution) — trust it over the
                // configured intent from session_info.
                ...(typeof data.model === 'string' && data.model
                  ? { model: data.model }
                  : {}),
              };
            });
            clearVisemeIdle();
            break;
          }

          case 'error':
            if (chatAccessMessageKey(data.code, data.reason) && (data.code === 'no_assigned_agent' || get().resumedExplicitly)) {
              set({ assignmentError: { code: data.code, message: data.message, reason: data.reason } });
            }
            if (targetTurnId && !isResumeNotFound(data.message)) {
              set(state => {
                const accessKey = chatAccessMessageKey(data.code, data.reason);
                const error = data.code === 'turn_limit' ? lookupMessage('webchat.turnLimit')
                  : data.code === 'attachment_upload_refused' ? lookupMessage('webchat.attachmentUploadRefused')
                  : accessKey ? lookupMessage(accessKey) : mentionErrorText(data) ?? data.message;
                const messages = finishMessage(state.messages, targetTurnId, data.code === 'cancelled' ? 'cancelled' : 'failed')
                  .map(m => m.role === 'user' && m.turnId === targetTurnId ? { ...m, ...(data.code === 'turn_limit' ? { deliveryError: error } : {}) } : m);
                // Admission errors are shown on the retained user bubble. Other
                // terminal notices follow this turn's reply, before queued users.
                if (data.code !== 'turn_limit') {
                  const slot = replySlot(messages, targetTurnId);
                  messages.splice(slot + (messages[slot]?.role === 'assistant' ? 1 : 0), 0,
                    { id: nextId(), role: 'system', content: error, timestamp: Date.now() });
                }
                return { messages,
                  ...(state.activeMessageId === targetTurnId || !state.activeMessageId
                    ? remainingActivity(messages, 'error') : {}),
                };
              });
              if (!get().activeMessageId) clearVisemeIdle();
              break;
            }
            if (isResumeNotFound(data.message)) {
              // Resume miss: the session we tried to continue was archived or
              // removed between listing and sending. Drop back to a fresh
              // conversation on the connection's own session, and tell the user
              // plainly (never swallow it).
              set({
                messages: [
                  {
                    id: nextId(),
                    role: 'system',
                    content: '找不到這個對話（可能已被封存或移除），已為你開啟新對話。',
                    timestamp: Date.now(),
                  },
                ],
                sessionId: get().ownSessionId,
                steps: [],
                stepTree: [],
                thinking: null,
                isStreaming: false,
                activeMessageId: null,
                phase: 'idle' as ChatPhase,
                viseme: REST_VISEME,
              });
              clearVisemeIdle();
              break;
            }
            set((state) => ({
              messages: [
                ...finishMessage(state.messages, targetTurnId, data.code === 'cancelled' ? 'cancelled' : 'failed'),
                {
                  id: nextId(),
                  role: 'system',
                  // `@` refusals and choices arrive as codes and are worded
                  // in the viewer's language here.
                  content: (chatAccessMessageKey(data.code, data.reason)
                    ? lookupMessage(chatAccessMessageKey(data.code, data.reason)!) : null)
                    ?? mentionErrorText(data) ?? data.message,
                  timestamp: Date.now(),
                },
              ],
              thinking: null,
              isStreaming: state.messages.some(m => m.delivery === 'queued'),
              activeMessageId: null,
              phase: 'error' as ChatPhase,
              viseme: REST_VISEME,
            }));
            clearVisemeIdle();
            break;
        }
      } catch {
        // Ignore malformed messages
      }
    };

    socket.onclose = () => {
      if (wsRef !== socket) return;
      if (wsRef === socket) wsRef = null;
      set({ connectionState: 'disconnected' });
      scheduleReconnect(connect);
    };

    socket.onerror = () => {
      if (wsRef !== socket) return;
      // onclose will fire after this — reconnect is handled there
    };
  }

  return {
    messages: [], lastCompletedReplyId: null,
    steps: [],
    stepTree: [],
    thinking: null,
    isStreaming: false,
    activeMessageId: null,
    lastTurnFrameAt: null,
    historyReplyRevision: 0,
    historyReconnectRevision: 0,
    phase: 'idle',
    viseme: REST_VISEME,
    sessionId: null,
    acceptedSessionId: null,
    ownSessionId: null,
    resumedExplicitly: false,
    convId: nextConvId(),
    sessionsRevision: 0,
    agentName: effectiveName(),
    // Empty until the server names an agent: the header then draws the
    // product mark (<BrandMark>), not the per-agent 🐾 placeholder.
    agentIcon: '',
    selectedAgentId: null,
    resolvedAgent: undefined,
    assignmentError: null,
    supportsVision: false,
    model: '',
    connectionState: 'disconnected',
    isRecording: false,
    isTranscribing: false,
    ttsEnabled: loadTtsEnabled(),

    connect,

    setRecording: (v: boolean) => set({ isRecording: v }),
    setTranscribing: (v: boolean) => set({ isTranscribing: v }),
    setTtsEnabled: (v: boolean) => {
      saveTtsEnabled(v);
      set({ ttsEnabled: v });
    },

    selectAgent: (id: string | null) => {
      // Switching the conversation partner starts a fresh thread: each employee
      // has an isolated server-side session (the gateway appends a per-agent
      // suffix), so clearing the local view keeps A's context out of B's. No
      // `/new` is sent — the sessions are already distinct server-side.
      if (id === get().selectedAgentId) return;
      clearVisemeIdle();
      set((state) => ({
        selectedAgentId: id,
        messages: [], lastCompletedReplyId: null,
        steps: [],
        stepTree: [],
        thinking: null,
        isStreaming: false,
        activeMessageId: null,
        phase: 'idle',
        viseme: REST_VISEME,
        // Fresh conversation with the new partner — bump the nonce so any reply
        // still in flight for the previous partner is dropped from this view.
        acceptedSessionId: null,
        convId: nextConvId(),
        // Restore the active session to this connection's own id. If we were
        // viewing a resumed historical conversation, its `sessionId` belongs to
        // the previous partner; keeping it would make the next send read as a
        // cross-agent resume and be rejected by the server's identity guard.
        sessionId: state.ownSessionId,
        resumedExplicitly: false,
      }));
    },

    applyHistoryCitations: (sessionId, rows) => {
      set((state) =>
        state.sessionId === sessionId
          ? { messages: attachHistoryCitations(state.messages, rows) }
          : {},
      );
    },

    discardHistory: (identityChanged = false) => {
      if (identityChanged) {
        // Explicit identity changes revoke accepted turns; ordinary transport
        // replacement must never send this control frame.
        if (wsRef && wsRef.readyState === WebSocket.OPEN) {
          try { wsRef.send(JSON.stringify({ type: 'logout' })); } catch { /* already disconnected */ }
        }
        get().disconnect();
      }
      clearVisemeIdle();
      set(state => ({
        messages: [], lastCompletedReplyId: null, steps: [], stepTree: [], thinking: null, isStreaming: false, activeMessageId: null,
        phase: 'idle', viseme: REST_VISEME, resumedExplicitly: false, acceptedSessionId: null, convId: nextConvId(),
        sessionId: identityChanged ? null : state.ownSessionId,
        ...(identityChanged ? { ownSessionId: null, selectedAgentId: null, resolvedAgent: undefined, assignmentError: null } : {}),
      }));
    },

    resumeSession: (sessionId: string, messages: readonly ChatMessage[]) => {
      clearVisemeIdle();
      set({
        sessionId,
        resumedExplicitly: true,
        messages: [...messages],
        lastCompletedReplyId: null,
        steps: [],
        stepTree: [],
        thinking: null,
        isStreaming: false,
        activeMessageId: null,
        phase: 'idle',
        viseme: REST_VISEME,
        // Fresh view token for the resumed conversation so a reply in flight for
        // the conversation we just left is dropped from this view. (The resumed
        // turn continues the stored session id server-side; `conv` here is only
        // the attribution tag, echoed back on reply frames.)
        acceptedSessionId: null,
        convId: nextConvId(),
      });
    },

    disconnect: () => {
      intentionalDisconnect = true;
      if (visibilityListener) document.removeEventListener('visibilitychange', visibilityListener);
      visibilityListener = null;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      reconnectAttempt = 0;
      if (wsRef) {
        wsRef.close();
        wsRef = null;
      }
      set({ connectionState: 'disconnected' });
    },

    send: (text: string, attachments?: readonly PendingAttachment[]) => {
      if (!wsRef || wsRef.readyState !== WebSocket.OPEN) return;
      if (requiresAssignedChatAgent() && (!get().resolvedAgent || get().assignmentError)) return;

      const atts = attachments ?? [];
      const msgId = newClientMessageId();

      // Add user message to local state immediately (with attachment chips)
      set((state) => ({
        acceptedSessionId: null,
        messages: [
          ...state.messages,
          {
            id: nextId(),
            role: 'user' as const,
            content: text,
            turnId: msgId,
            delivery: state.isStreaming ? 'queued' as const : 'running' as const,
            timestamp: Date.now(),
            attachments: atts.map((a) => ({ name: a.name, mime: a.mime })),
          },
        ],
        ...(!state.isStreaming ? {
          steps: [], // fresh task-insight timeline for this turn
          stepTree: [], // fresh tool step tree for this turn
          // The timer starts now, for every runtime — it never waits on the
          // provider reporting thinking.
          thinking: startTurnThinking(Date.now()),
          isStreaming: true,
          activeMessageId: msgId,
          lastTurnFrameAt: Date.now(),
          phase: 'thinking' as ChatPhase,
          viseme: REST_VISEME,
        } : {}),
      }));
      if (get().activeMessageId === msgId) clearVisemeIdle();

      wsRef.send(
        JSON.stringify(
          buildUserMessageFrame({
            content: text,
            sessionId: get().sessionId,
            agentId: requiresAssignedChatAgent() && !get().resumedExplicitly ? null : get().selectedAgentId,
            attachments: atts,
            convId: get().convId,
            msgId,
          }),
        ),
      );

      // The user message is what creates/updates the server-side session
      // bucket — the conversation should appear in the list as soon as the
      // input lands, not only after the reply completes. The sessions.list
      // RPC travels over a different socket than the chat frame, so a
      // just-sent message may not be persisted yet when an immediate fetch
      // arrives; the first server frame for this turn bumps again as a
      // persistence-confirmed refresh (see onmessage).
      awaitingFirstServerFrame = true;
      set((state) => ({ sessionsRevision: state.sessionsRevision + 1 }));
    },

    cancel: (msgId = get().activeMessageId ?? undefined) => {
      if (!msgId || !wsRef || wsRef.readyState !== WebSocket.OPEN) return;
      wsRef.send(JSON.stringify({ type: 'cancel', request_id: newClientMessageId(),
        conv: get().convId, msg_id: msgId }));
    },

    reset: () => {
      const { ownSessionId, sessionId } = get();
      // "New conversation" simply starts a fresh conversation nonce: the next
      // message lands in a brand-new server-side session bucket
      // (`…#conv:<newNonce>`), which is empty, so the AI starts fresh — while the
      // PREVIOUS conversation is preserved (and resumable from the list), not
      // deleted. It deliberately does NOT send `/new`: (1) a destructive
      // delete_session would wipe a conversation whose long task may still be
      // running, and (2) that in-flight reply, echoed with the OLD nonce, is now
      // dropped from this view by the attribution guard instead of leaking here.
      // The active session id points back at this connection's own base id so the
      // next send is a new-conversation turn (not a resume of a historical one).
      const target = ownSessionId ?? sessionId;
      clearVisemeIdle();
      set({
        messages: [], lastCompletedReplyId: null,
        steps: [],
        stepTree: [],
        thinking: null,
        isStreaming: false,
        activeMessageId: null,
        phase: 'idle',
        viseme: REST_VISEME,
        sessionId: target,
        resumedExplicitly: false,
        acceptedSessionId: null,
        convId: nextConvId(),
      });
    },
  };
});

// View changes replace the socket subscription; they never cancel accepted turns.
useChatStore.subscribe((next, previous) => {
  if (next.convId !== previous.convId && next.connectionState === 'connected') {
    subscribeCurrentConversation();
  }
});
