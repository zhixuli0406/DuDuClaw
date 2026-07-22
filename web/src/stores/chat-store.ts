import { create } from 'zustand';
import { useAuthStore } from './auth-store';
import type { VisemeShape } from '@/components/mascot';
import { REST_VISEME, sampleViseme } from '@/components/chat/viseme-sampler';
import { loadTtsEnabled, saveTtsEnabled } from '@/components/chat/tts-playback';
import { effectiveName, effectiveLogoGlyph } from '@/lib/branding';

export interface ChatAttachmentMeta {
  readonly name: string;
  readonly mime: string;
}

export interface ChatMessage {
  readonly id: string;
  readonly role: 'user' | 'assistant' | 'system';
  readonly content: string;
  readonly timestamp: number;
  readonly tokens?: number;
  /** Files attached to a user message (display chips). */
  readonly attachments?: readonly ChatAttachmentMeta[];
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

/** Where the assistant is in the current turn — drives DuDu's face (V7/T7.1). */
export type ChatPhase = 'idle' | 'thinking' | 'speaking' | 'done' | 'error';

interface ChatStore {
  readonly messages: readonly ChatMessage[];
  /** Live task-board insight steps for the current turn (cleared on each send). */
  readonly steps: readonly ChatStep[];
  /** Live tool step tree for the current turn (folded from `step` frames). */
  readonly stepTree: readonly StepNode[];
  readonly isStreaming: boolean;
  /** Fine-grained turn phase for the companion face (thinking/speaking/…). */
  readonly phase: ChatPhase;
  /** Current mouth shape while `speaking` — sampled per assistant chunk. */
  readonly viseme: VisemeShape;
  readonly sessionId: string | null;
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
  setRecording: (v: boolean) => void;
  setTranscribing: (v: boolean) => void;
  setTtsEnabled: (v: boolean) => void;
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
}): Record<string, unknown> {
  const { content, sessionId, agentId, attachments, convId } = opts;
  return {
    type: 'user_message',
    content,
    session_id: sessionId,
    ...(agentId ? { agent: agentId } : {}),
    ...(convId ? { conv: convId } : {}),
    attachments: attachments.map((a) => ({
      filename: a.name,
      mime: a.mime,
      data_base64: a.dataBase64,
    })),
  };
}

/** The wire shape of one message returned by `chat.sessions.history`. */
export interface HistoryMessageWire {
  readonly role: string;
  readonly content: string;
  /** RFC3339 timestamp. */
  readonly timestamp: string;
  readonly tokens?: number;
}

/**
 * Map history-RPC messages into the store's `ChatMessage` shape (pure — exported
 * for tests). Unknown roles collapse to `user`; an unparseable timestamp falls
 * back to "now" so a bad row never breaks the render.
 */
export function historyToMessages(raw: readonly HistoryMessageWire[]): ChatMessage[] {
  return raw.map((m) => {
    const role: ChatMessage['role'] =
      m.role === 'assistant' ? 'assistant' : m.role === 'system' ? 'system' : 'user';
    const ts = Date.parse(m.timestamp);
    return {
      id: nextId(),
      role,
      content: m.content ?? '',
      timestamp: Number.isFinite(ts) ? ts : Date.now(),
      ...(typeof m.tokens === 'number' ? { tokens: m.tokens } : {}),
    };
  });
}

/** True when a `/ws/chat` error frame reports a resume miss (the session the
 *  client tried to continue no longer exists). Pure — exported for tests. */
export function isResumeNotFound(message: string | null | undefined): boolean {
  return /conversation not found/i.test(String(message ?? ''));
}

/**
 * Decide whether a reply frame belongs to the conversation currently open, so a
 * long task started in conversation A doesn't render into a conversation B the
 * user switched to while it ran. Pure — exported for tests.
 *
 *  - An untagged frame (`conv` absent — legacy gateway) is always accepted.
 *  - A tagged frame is accepted only when its `conv` equals `currentConvId`;
 *    otherwise it belongs to another conversation and is dropped from the view
 *    (the gateway has already persisted it to that conversation's own history).
 */
export function frameBelongsToConversation(frameConv: unknown, currentConvId: string): boolean {
  if (typeof frameConv !== 'string' || frameConv.length === 0) return true;
  return frameConv === currentConvId;
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
    if (wsRef && wsRef.readyState === WebSocket.OPEN) return;

    intentionalDisconnect = false;
    // A fresh connection re-derives its own server session id, so start a fresh
    // conversation nonce too (the previous connection's buckets are unreachable
    // once the server mints a new suffix).
    set({ connectionState: 'connecting', ownSessionId: null, convId: nextConvId() });

    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${protocol}//${window.location.host}/ws/chat`;
    const socket = new WebSocket(url);
    wsRef = socket;

    socket.onopen = () => {
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
      try {
        const data = JSON.parse(event.data);
        // A finished reply — for ANY conversation, even one whose frames are
        // about to be dropped below — changed the server-side session list
        // (created/updated a session bucket). Signal the page to refresh the
        // list so the conversation becomes resumable right away.
        if (data.type === 'assistant_done') {
          set((state) => ({ sessionsRevision: state.sessionsRevision + 1 }));
        } else if (
          awaitingFirstServerFrame &&
          (data.type === 'assistant_chunk' ||
            data.type === 'step' ||
            data.type === 'progress')
        ) {
          // First server frame after a send: the user message is now
          // persisted server-side — refresh the list once (persistence-
          // confirmed complement to the optimistic bump in `send`).
          awaitingFirstServerFrame = false;
          set((state) => ({ sessionsRevision: state.sessionsRevision + 1 }));
        }
        // Attribution guard: drop reply frames (chunk/step/progress/done) that
        // belong to a conversation the user has since left. `session_info` and
        // `error` are connection-level (untagged) and pass through. Without this,
        // a long task from conversation A would render into conversation B when it
        // finishes after the user opened B — the reported bug.
        if (
          (data.type === 'assistant_chunk' ||
            data.type === 'step' ||
            data.type === 'progress' ||
            data.type === 'assistant_done') &&
          !frameBelongsToConversation(data.conv, get().convId)
        ) {
          return;
        }
        switch (data.type) {
          case 'session_info':
            // `refresh: true` marks a server-initiated re-announcement after an
            // agent-config change (name/icon/model edited in the dashboard).
            // Update only the agent metadata — the session state (sessionId /
            // ownSessionId) belongs to this client's navigation and must not
            // be clobbered mid-conversation.
            if (data.refresh) {
              set({
                agentName: data.agent_name,
                agentIcon: data.agent_icon,
                supportsVision: data.supports_vision ?? false,
                model: data.model ?? '',
              });
              break;
            }
            // The first session_info after a fresh connect is this connection's
            // own session — remember it so a later resume miss / new-conversation
            // / partner-switch can fall back to it. Resume-confirmation frames (a
            // different session_id) must not overwrite this baseline.
            set((state) => ({
              ownSessionId:
                state.ownSessionId === null && typeof data.session_id === 'string'
                  ? data.session_id
                  : state.ownSessionId,
              sessionId: data.session_id,
              agentName: data.agent_name,
              agentIcon: data.agent_icon,
              supportsVision: data.supports_vision ?? false,
              model: data.model ?? '',
            }));
            break;

          case 'assistant_chunk':
            // Streaming chunk — append to last assistant message or create new.
            // Sample the mouth shape off the chunk cadence (T7.1) and arm the
            // idle timer so a stream pause relaxes the mouth back to REST.
            set((state) => {
              const msgs = [...state.messages];
              const last = msgs[msgs.length - 1];
              if (last && last.role === 'assistant' && !('tokens' in last)) {
                msgs[msgs.length - 1] = {
                  ...last,
                  content: last.content + data.content,
                };
              } else {
                msgs.push({
                  id: nextId(),
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

          case 'assistant_done':
            set((state) => {
              const msgs = [...state.messages];
              const last = msgs[msgs.length - 1];
              if (last && last.role === 'assistant') {
                // Update with full content + tokens
                msgs[msgs.length - 1] = {
                  ...last,
                  content: data.content,
                  tokens: data.tokens_used,
                };
              } else {
                msgs.push({
                  id: nextId(),
                  role: 'assistant',
                  content: data.content,
                  timestamp: Date.now(),
                  tokens: data.tokens_used,
                });
              }
              return {
                messages: msgs,
                isStreaming: false,
                phase: 'done' as ChatPhase,
                viseme: REST_VISEME,
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

          case 'error':
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
                    content: '⚠️ 找不到這個對話（可能已被封存或移除），已為你開啟新對話。',
                    timestamp: Date.now(),
                  },
                ],
                sessionId: get().ownSessionId,
                steps: [],
                stepTree: [],
                isStreaming: false,
                phase: 'idle' as ChatPhase,
                viseme: REST_VISEME,
              });
              clearVisemeIdle();
              break;
            }
            set((state) => ({
              messages: [
                ...state.messages,
                {
                  id: nextId(),
                  role: 'system',
                  content: `⚠️ ${data.message}`,
                  timestamp: Date.now(),
                },
              ],
              isStreaming: false,
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
      if (wsRef === socket) wsRef = null;
      set({ connectionState: 'disconnected' });
      scheduleReconnect(connect);
    };

    socket.onerror = () => {
      // onclose will fire after this — reconnect is handled there
    };
  }

  return {
    messages: [],
    steps: [],
    stepTree: [],
    isStreaming: false,
    phase: 'idle',
    viseme: REST_VISEME,
    sessionId: null,
    ownSessionId: null,
    convId: nextConvId(),
    sessionsRevision: 0,
    agentName: effectiveName(),
    agentIcon: effectiveLogoGlyph(),
    selectedAgentId: null,
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
        messages: [],
        steps: [],
        stepTree: [],
        isStreaming: false,
        phase: 'idle',
        viseme: REST_VISEME,
        // Fresh conversation with the new partner — bump the nonce so any reply
        // still in flight for the previous partner is dropped from this view.
        convId: nextConvId(),
        // Restore the active session to this connection's own id. If we were
        // viewing a resumed historical conversation, its `sessionId` belongs to
        // the previous partner; keeping it would make the next send read as a
        // cross-agent resume and be rejected by the server's identity guard.
        sessionId: state.ownSessionId,
      }));
    },

    resumeSession: (sessionId: string, messages: readonly ChatMessage[]) => {
      clearVisemeIdle();
      set({
        sessionId,
        messages: [...messages],
        steps: [],
        stepTree: [],
        isStreaming: false,
        phase: 'idle',
        viseme: REST_VISEME,
        // Fresh view token for the resumed conversation so a reply in flight for
        // the conversation we just left is dropped from this view. (The resumed
        // turn continues the stored session id server-side; `conv` here is only
        // the attribution tag, echoed back on reply frames.)
        convId: nextConvId(),
      });
    },

    disconnect: () => {
      intentionalDisconnect = true;
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

      const atts = attachments ?? [];

      // Add user message to local state immediately (with attachment chips)
      set((state) => ({
        messages: [
          ...state.messages,
          {
            id: nextId(),
            role: 'user' as const,
            content: text,
            timestamp: Date.now(),
            attachments: atts.map((a) => ({ name: a.name, mime: a.mime })),
          },
        ],
        steps: [], // fresh task-insight timeline for this turn
        stepTree: [], // fresh tool step tree for this turn
        isStreaming: true,
        phase: 'thinking' as ChatPhase,
        viseme: REST_VISEME,
      }));
      clearVisemeIdle();

      wsRef.send(
        JSON.stringify(
          buildUserMessageFrame({
            content: text,
            sessionId: get().sessionId,
            agentId: get().selectedAgentId,
            attachments: atts,
            convId: get().convId,
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
        messages: [],
        steps: [],
        stepTree: [],
        isStreaming: false,
        phase: 'idle',
        viseme: REST_VISEME,
        sessionId: target,
        convId: nextConvId(),
      });
    },
  };
});
