import { create } from 'zustand';
import { api, type ChatSessionHistory, type ChatSessionSummary } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { isConversationSession } from '@/lib/session-channel';
import { hasMinRole } from '@/lib/roles';
import { useAgentsStore } from './agents-store';
import { useAuthStore } from './auth-store';
import { useChatStore, historyToMessages } from './chat-store';
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
function identityKey(): string {
  const { user, bindings } = useAuthStore.getState();
  return JSON.stringify([user?.id, user?.role, bindings.map(b => [b.agent_name, b.access_level]).sort()]);
}
function scopeKey(): string {
  return JSON.stringify([identityKey(), useChatStore.getState().selectedAgentId ?? mainAgentId()]);
}
function validExternal(history: ChatSessionHistory): boolean {
  if (history.privacy_mode !== 'masked' || typeof history.ner !== 'boolean' || !history.channel || history.channel==='webchat' || !isConversationSession(history.session_id,history.channel)) return false;
  return history.messages.every(message => (message.role === 'user' || message.role === 'assistant') && typeof message.timestamp==='string' && Number.isFinite(Date.parse(message.timestamp)) && (message.sender_label===undefined || typeof message.sender_label==='string') && Array.isArray(message.parts) && message.parts.every(part => {
    if (part.t === 'text') return typeof part.v === 'string';
    if (part.t === 'mask') return typeof part.cat === 'string' && Number.isInteger(part.n) && part.n > 0;
    return part.t === 'attachment' && (part.kind === 'file' || part.kind === 'image') && !part.name;
  }));
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
        const agentId = useChatStore.getState().selectedAgentId ?? mainAgentId();
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
        try { useChatStore.getState().selectAgent(owner === mainAgentId() ? null : owner); }
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
    refreshView: async () => { if (lastTarget) await get().resume(lastTarget); },
    startNew: () => {
      ++resumeEpoch; lastTarget = null;
      if (get().viewOnly) useChatStore.getState().discardHistory();
      set({ ...emptyView });
      useChatStore.getState().reset();
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
  const fingerprint = (state: typeof next) => JSON.stringify([state.user?.id, state.user?.role, state.bindings.map(b => [b.agent_name, b.access_level]).sort()]);
  if (fingerprint(next) === fingerprint(previous)) return;
  if (next.user?.id !== previous.user?.id) {
    useChatStore.getState().disconnect();
    useChatStore.getState().discardHistory(true);
  } else if (useChatStore.getState().resumedExplicitly) {
    useChatStore.getState().discardHistory();
  }
  // The interactive pane may mount as soon as viewOnly becomes false.
  // Erase historical chat state before releasing that protected pane.
  invalidateViews();
});
useChatStore.subscribe((next, previous) => {
  if (next.selectedAgentId !== previous.selectedAgentId && !selectingForResume) invalidateViews();
});
