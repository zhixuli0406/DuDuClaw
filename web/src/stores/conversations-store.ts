import { create } from 'zustand';
import { api, type ChatSessionSummary } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { isConversationSession } from '@/lib/session-channel';
import { hasMinRole } from '@/lib/roles';
import { useAgentsStore } from './agents-store';
import { useAuthStore } from './auth-store';
import { useChatStore, historyToMessages } from './chat-store';

/**
 * Conversation history — the single source for BOTH the sidebar 對話紀錄 group
 * and the chat page's left column (2026-07-30 client feedback round 4).
 *
 * Before this store each surface owned a private copy of "list sessions +
 * resume one", which is how the two would have drifted. The listing itself is
 * scoped exactly as `chat.sessions.list` demands: admins may enumerate every
 * agent, everyone else MUST pass a bound `agent_id` (the RPC is fail-closed).
 * The server returns newest-first, already excluding archived sessions; the
 * client only drops internal work sessions (cron / delegation / goal runs) via
 * {@link isConversationSession}.
 */

/** Admins list across all agents; the rest stay scoped to one employee. */
const ADMIN_LIMIT = 100;
const SCOPED_LIMIT = 50;

export type ConversationsStatus = 'idle' | 'loading' | 'error' | 'ready';

interface ConversationsState {
  readonly sessions: readonly ChatSessionSummary[];
  readonly status: ConversationsStatus;
  /** Reload the list. Concurrent calls collapse onto one in-flight request. */
  fetch: () => Promise<void>;
  /**
   * Load a past conversation into the chat view. Returns false when the
   * transcript could not be fetched — the caller decides how to surface that
   * (toast on the chat page, silent no-op elsewhere).
   */
  resume: (session: ChatSessionSummary) => Promise<boolean>;
  /** Start a fresh conversation (keeps the previous one resumable). */
  startNew: () => void;
}

/** Collapses simultaneous fetches from the sidebar + the chat page into one. */
let inFlight: Promise<void> | null = null;

/** The `main` agent answers when no employee is explicitly selected. */
function mainAgentId(): string | null {
  return useAgentsStore.getState().agents.find((a) => a.role === 'main')?.name ?? null;
}

export const useConversationsStore = create<ConversationsState>((set, get) => {
  // Channel conversations (Telegram/Discord/…) land server-side without any
  // webchat-local trigger; the gateway broadcasts `chat.sessions.updated`
  // after each saved turn so this list stays live without a reload.
  client.subscribe('chat.sessions.updated', () => {
    void get().fetch();
  });

  return {
  sessions: [],
  status: 'idle',

  fetch: () => {
    if (inFlight) return inFlight;
    inFlight = (async () => {
      const canListAll = hasMinRole(useAuthStore.getState().user?.role, 'admin');
      const agentId = useChatStore.getState().selectedAgentId ?? mainAgentId();
      // Non-admin with nothing to scope to: the RPC would reject an unscoped
      // listing, so answer honestly with an empty list instead of an error.
      if (!canListAll && !agentId) {
        set({ sessions: [], status: 'ready' });
        return;
      }
      set({ status: 'loading' });
      try {
        const res = await api.chatSessions.list(
          canListAll ? { limit: ADMIN_LIMIT } : { agent_id: agentId!, limit: SCOPED_LIMIT },
        );
        set({
          sessions: (res?.sessions ?? []).filter((s) => isConversationSession(s.session_id)),
          status: 'ready',
        });
      } catch {
        set({ status: 'error' });
      }
    })().finally(() => {
      inFlight = null;
    });
    return inFlight;
  },

  resume: async (session: ChatSessionSummary) => {
    try {
      const hist = await api.chatSessions.history(session.session_id);
      // Cross-channel sessions belong to whichever employee handles that
      // channel — switch the partner first so the server's cross-agent resume
      // guard accepts the follow-up turn (the main agent maps to the DuDu chip,
      // i.e. no explicit selection).
      const owner = hist.agent_id || session.agent_id;
      const chat = useChatStore.getState();
      if (owner) chat.selectAgent(owner === mainAgentId() ? null : owner);
      // Re-read: `selectAgent` replaced the store slice above.
      useChatStore.getState().resumeSession(session.session_id, historyToMessages(hist.messages ?? []));
      return true;
    } catch {
      return false;
    }
  },

  startNew: () => {
    useChatStore.getState().reset();
    void get().fetch();
  },
  };
});
