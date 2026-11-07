import { create } from 'zustand';
import { api, type ChatSessionHistory, type ChatSessionSummary } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { isConversationSession } from '@/lib/session-channel';
import { hasMinRole } from '@/lib/roles';
import { useAgentsStore } from './agents-store';
import { useAuthStore } from './auth-store';
import { useChatStore, historyToMessages, requiresAssignedChatAgent, type ChatMessage } from './chat-store';
import { REST_VISEME } from '@/components/chat/viseme-sampler';
import { fetchSessionCitations } from '@/lib/knowledge-citations';

const ADMIN_LIMIT = 100;
const SCOPED_LIMIT = 50;
export type ConversationsStatus = 'idle' | 'loading' | 'error' | 'ready';
interface ConversationsState {
  readonly sessions: readonly ChatSessionSummary[];
  readonly status: ConversationsStatus;
  readonly viewOnly: boolean;
  readonly viewLoading: boolean;
  readonly viewError: boolean;
  readonly readOnlyView: ChatSessionHistory | null;
  fetch: () => Promise<void>;
  resume: (session: ChatSessionSummary) => Promise<boolean>;
  refreshView: () => Promise<void>;
  refreshReconnected: () => Promise<void>;
  refreshUnanswered: (isCurrent: () => boolean) => Promise<void>;
  startNew: () => void;
}
let fetchEpoch = 0;
let resumeEpoch = 0;
const inFlight = new Map<string, Promise<void>>();
let lastTarget: ChatSessionSummary | null = null;
let selectingForResume = false;
function mainAgentId(): string | null {
  return useAgentsStore.getState().agents.find(a => a.role === 'main')?.name ?? null;
}
/** Every history cache uses the same complete authenticated subject. */
export function conversationIdentityKey(state = useAuthStore.getState()): string {
  const { user, bindings } = state;
  return JSON.stringify([user?.id, user?.role, user?.status, bindings.map(b => [b.agent_name, b.access_level]).sort()]);
}
function historyAgentId(): string | null {
  const chat = useChatStore.getState();
  return requiresAssignedChatAgent() ? chat.resolvedAgent?.id ?? null : chat.selectedAgentId ?? mainAgentId();
}
function scopeKey(): string {
  return JSON.stringify([conversationIdentityKey(), historyAgentId()]);
}
function validExternal(history: ChatSessionHistory): boolean {
  if (history.privacy_mode !== 'masked' || typeof history.ner !== 'boolean' || !history.channel || history.channel==='webchat' || !isConversationSession(history.session_id,history.channel)) return false;
  return history.messages.every(message => (message.role === 'user' || message.role === 'assistant') && typeof message.timestamp==='string' && Number.isFinite(Date.parse(message.timestamp)) && (message.sender_label===undefined || typeof message.sender_label==='string') && Array.isArray(message.parts) && message.parts.every(part => {
    if (part.t === 'text') return typeof part.v === 'string';
    if (part.t === 'mask') return typeof part.cat === 'string' && Number.isInteger(part.n) && part.n > 0;
    return part.t === 'attachment' && (part.kind === 'file' || part.kind === 'image') && !part.name;
  }));
}
/** Call only after checking the exact history session. A lost acknowledgement
 * can recover through the client id persisted with the user row. */
function storedUserIndex(local: ChatMessage, rows: readonly { id?: number; role: string; client_msg_id?: string | null }[]): number {
  if (local.historyId !== undefined) return rows.findIndex(row => row.role === 'user' && row.id === local.historyId);
  if (typeof local.turnId !== 'string' || !local.turnId) return -1;
  let match = -1;
  for (let index = 0; index < rows.length; index += 1) {
    if (rows[index].role !== 'user' || rows[index].client_msg_id !== local.turnId) continue;
    if (match !== -1) return -1;
    match = index;
  }
  return match;
}

const emptyView = { viewOnly: false, viewLoading: false, viewError: false, readOnlyView: null } as const;
export const useConversationsStore = create<ConversationsState>((set, get) => {
  client.subscribe('chat.sessions.updated', () => { void get().fetch(); });
  return {
    sessions: [], status: 'idle', ...emptyView,
    fetch: () => {
      const key = scopeKey();
      const existing = inFlight.get(key);
      if (existing) return existing;
      const ticket = ++fetchEpoch;
      const request = (async () => {
        const canListAll = hasMinRole(useAuthStore.getState().user?.role, 'admin');
        const agentId = historyAgentId();
        set({ sessions: [], status: 'loading' });
        if (!canListAll && !agentId) { set({ status: 'ready' }); return; }
        try {
          const res = await api.chatSessions.list(canListAll ? { limit: ADMIN_LIMIT } : { agent_id: agentId!, limit: SCOPED_LIMIT });
          if (ticket !== fetchEpoch || key !== scopeKey()) return;
          set({ sessions: (res?.sessions ?? []).filter(s => isConversationSession(s.session_id, s.channel)), status: 'ready' });
        } catch {
          if (ticket === fetchEpoch && key === scopeKey()) set({ sessions: [], status: 'error' });
        }
      })().finally(() => { if (inFlight.get(key) === request) inFlight.delete(key); });
      inFlight.set(key, request);
      return request;
    },
    resume: async (session) => {
      const ticket = ++resumeEpoch;
      const key = scopeKey();
      lastTarget = session;
      set({ viewOnly: true, viewLoading: true, viewError: false, readOnlyView: null });
      try {
        const hist = await api.chatSessions.history(session.session_id);
        if (ticket !== resumeEpoch || key !== scopeKey()) return false;
        if (!hist || hist.agent_id !== session.agent_id || !Array.isArray(hist.messages)) throw new Error('invalid history');
        const opaque = /^view:v1:s:[a-f0-9]{64}$/.test(hist.session_id);
        const ownAlias = /^view:v1:s:[a-f0-9]{64}$/.test(session.session_id) && hist.privacy_mode==='own' && hist.channel==='webchat' && !hist.read_only;
        if (hist.session_id !== session.session_id && !ownAlias && (!hist.read_only || !opaque || session.session_id.startsWith('view:'))) throw new Error('invalid history');
        const owner = hist.agent_id;
        selectingForResume = true;
        try {
          useChatStore.getState().selectAgent(!requiresAssignedChatAgent() && owner === mainAgentId() ? null : owner);
          if (requiresAssignedChatAgent() && !hist.read_only) {
            const agent = useAgentsStore.getState().agents.find(a => a.name === owner);
            useChatStore.setState({
              resolvedAgent: { id: owner, name: agent?.display_name ?? owner, icon: agent?.icon ?? '' },
              agentName: agent?.display_name ?? owner,
              agentIcon: agent?.icon ?? '',
              assignmentError: agent?.role === 'main'
                ? { code: 'main_agent_admin_only', message: '' } : null,
            });
          }
        }
        finally { selectingForResume = false; }
        if (hist.read_only === true) {
          if (!opaque || !validExternal(hist)) throw new Error('invalid masked history');
          // Keep the interactive socket on its own session. A view handle is
          // never a resume target or an authority for citations/action cards.
          lastTarget = { ...session, session_id: hist.session_id, title: '', read_only: true };
          set({ viewOnly: true, viewLoading: false, viewError: false, readOnlyView: hist });
        } else {
          useChatStore.getState().resumeSession(hist.session_id, historyToMessages(hist.messages));
          set({ ...emptyView });
          const id = hist.session_id;
          void fetchSessionCitations(id).then(rows => {
            if (ticket === resumeEpoch && rows.length > 0) useChatStore.getState().applyHistoryCitations(id, rows);
          });
        }
        return true;
      } catch {
        if (ticket === resumeEpoch) set({ viewOnly: true, viewLoading: false, viewError: true, readOnlyView: null });
        return false;
      }
    },
    refreshReconnected: async () => {
      const before = useChatStore.getState();
      const { sessionId, convId, messages } = before;
      if (!sessionId || !before.resumedExplicitly || get().viewOnly) return;
      const ticket = resumeEpoch;
      const key = scopeKey();
      const stillCurrent = () => {
        const now = useChatStore.getState();
        return ticket === resumeEpoch && key === scopeKey() && !get().viewOnly
          && now.sessionId === sessionId && now.convId === convId && now.messages === messages;
      };
      // The RPC transport reconnects independently of the chat socket. Retry
      // brief handshake/outage failures without overwriting a later view.
      for (let attempt = 0; attempt < 3 && stillCurrent(); attempt += 1) {
        try {
          const hist = await api.chatSessions.history(sessionId);
          if (!stillCurrent() || !hist || hist.session_id !== sessionId || hist.read_only
            || !Array.isArray(hist.messages)) return;
          // Never replace a newly sent message with a snapshot taken before its
          // acceptance. Requests from another account/view are fenced above.
          const lastUser = messages.findLast(message => message.role === 'user');
          if (lastUser && storedUserIndex(lastUser, hist.messages) < 0) break;
          const updated = historyToMessages(hist.messages);
          // A disconnected stream has no reliable progress state. An unanswered
          // persisted user row uses the existing bounded history poll instead.
          useChatStore.setState({ messages: updated, isStreaming: false, activeMessageId: null,
            thinking: null, steps: [], stepTree: [], phase: 'idle', viseme: REST_VISEME, lastCompletedReplyId: null });
          void fetchSessionCitations(sessionId).then(rows => {
            const now = useChatStore.getState();
            if (ticket === resumeEpoch && key === scopeKey() && now.sessionId === sessionId
              && now.convId === convId && now.messages === updated && rows.length) now.applyHistoryCitations(sessionId, rows);
          }).catch(() => {});
          return;
        } catch {
          if (!stillCurrent()) return;
          if (attempt < 2) await new Promise(resolve => setTimeout(resolve, 1_000 * (attempt + 1)));
        }
      }
      // Lost transport progress must not remain an endless thinking indicator.
      // A retained unanswered row falls back to the existing bounded poll.
      if (stillCurrent()) useChatStore.setState({ isStreaming: false, activeMessageId: null,
        thinking: null, steps: [], stepTree: [], phase: 'idle', viseme: REST_VISEME });
    },
    refreshUnanswered: async (isCurrent) => {
      const before = useChatStore.getState();
      const { sessionId, convId, messages } = before;
      const last = messages.at(-1);
      if (!isCurrent() || !sessionId || !before.resumedExplicitly || before.isStreaming || before.activeMessageId || last?.role !== 'user' || get().viewOnly) return;
      const ticket = resumeEpoch;
      const key = scopeKey();
      const stillCurrent = () => {
        const now = useChatStore.getState();
        return isCurrent() && ticket === resumeEpoch && key === scopeKey() && !get().viewOnly
          && now.sessionId === sessionId && now.convId === convId && now.messages === messages
          && now.resumedExplicitly && !now.isStreaming && !now.activeMessageId;
      };
      try {
        const hist = await api.chatSessions.history(sessionId);
        if (!stillCurrent() || !hist || hist.session_id !== sessionId || hist.read_only
          || !Array.isArray(hist.messages)) return;
        // Require the unanswered row and a later reply or failure notice. A stale or
        // truncated snapshot must not replace a newer transcript.
        const anchor = storedUserIndex(last, hist.messages);
        if (anchor < 0 || !hist.messages.slice(anchor + 1).some(row => row.role === 'assistant' || row.notice_kind === 'turn_failure')) return;
        const updated = historyToMessages(hist.messages);
        useChatStore.setState({ messages: updated });
        void fetchSessionCitations(sessionId).then(rows => {
          const now = useChatStore.getState();
          if (ticket === resumeEpoch && key === scopeKey() && now.sessionId === sessionId && now.convId === convId
            && now.messages === updated && !now.isStreaming && !now.activeMessageId && rows.length) {
            now.applyHistoryCitations(sessionId, rows);
          }
        }).catch(() => {});
      } catch {
        // The bounded poll retries; retain the unanswered notice and draft.
      }
    },
    refreshView: async () => { if (lastTarget) await get().resume(lastTarget); },
    startNew: () => {
      ++resumeEpoch; lastTarget = null;
      if (get().viewOnly) useChatStore.getState().discardHistory();
      set({ ...emptyView });
      const previousChat = useChatStore.getState();
      useChatStore.getState().reset();
      if (requiresAssignedChatAgent()) useChatStore.getState().selectAgent(null);
      if (!requiresAssignedChatAgent()) {
        const primary = useAgentsStore.getState().agents.find(agent =>
          agent.name === useAuthStore.getState().user?.primary_agent_id);
        if (primary) useChatStore.getState().selectAgent(primary.role === 'main' ? null : primary.name);
      }
      const chat = useChatStore.getState();
      if (requiresAssignedChatAgent() && (!chat.resolvedAgent || chat.assignmentError
        || (previousChat.resumedExplicitly && !previousChat.isStreaming))
        && chat.connectionState !== 'disconnected') {
        useChatStore.getState().disconnect();
        useChatStore.getState().connect();
      }
      void get().fetch();
    },
  };
});

function invalidateViews() {
  ++fetchEpoch; ++resumeEpoch; lastTarget = null;
  // An earlier promise must not collapse a new request after a privilege or
  // account transition back to the same scope string.
  inFlight.clear();
  useConversationsStore.setState({ sessions: [], status: 'idle', ...emptyView });
}
useAuthStore.subscribe((next, previous) => {
  const identityChanged = next.user?.id !== previous.user?.id || (!!previous.jwt && !next.jwt);
  if (!identityChanged && conversationIdentityKey(next) === conversationIdentityKey(previous)) return;
  if (identityChanged) {
    useChatStore.getState().discardHistory(true);
  } else if (useChatStore.getState().resumedExplicitly) {
    useChatStore.getState().discardHistory();
  }
  // The interactive pane may mount as soon as viewOnly becomes false.
  // Erase historical chat state before releasing that protected pane.
  invalidateViews();
});
useChatStore.subscribe((next, previous) => {
  if (next.historyReconnectRevision !== previous.historyReconnectRevision
    || (next.historyReplyRevision !== previous.historyReplyRevision && !next.isStreaming
      && next.messages.at(-1)?.role !== 'user')) void useConversationsStore.getState().refreshReconnected();
  if ((next.selectedAgentId !== previous.selectedAgentId || next.resolvedAgent?.id !== previous.resolvedAgent?.id) && !selectingForResume) invalidateViews();
});
