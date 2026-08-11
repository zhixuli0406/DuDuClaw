import { useNavigate } from 'react-router';
import { idMatchRoute, type IdMatch } from '@/lib/id-lookup';
import { useConversationsStore } from '@/stores/conversations-store';
import { useDevPanelStore } from '@/stores/devpanel-store';

/**
 * Devpanel object-id navigation (W3-4) — reuses the W3-3 id-lookup route
 * table (`@/lib/id-lookup`) instead of inventing a second one.
 *
 * The command palette's id-lookup has to fan out to several list RPCs
 * because it doesn't know what *kind* of id the user pasted. Everything
 * shown inside this panel already carries its kind from the RPC that
 * produced it — an `agent_id` field is always an agent, a takeover record's
 * `conversation` is always a conversation — so this skips straight to
 * `idMatchRoute` (or the conversation-resume flow) instead of re-deriving it.
 */
export function useDevPanelIdJump() {
  const navigate = useNavigate();
  const minimize = useDevPanelStore((s) => s.minimize);

  const jump = (match: IdMatch): boolean => {
    const route = idMatchRoute(match);
    if (!route) return false;
    // Get out of the way of the page being navigated to — same reasoning as
    // the command palette closing itself on a hit.
    minimize();
    navigate(route);
    return true;
  };

  const jumpToAgent = (agentId: string): void => {
    jump({ kind: 'agent', id: agentId });
  };

  const jumpToTask = (taskId: string): void => {
    jump({ kind: 'task', id: taskId });
  };

  /**
   * Conversation ids have no plain route (W3-3, `idMatchRoute` returns
   * `null` for `conversation` on purpose) — resuming means replaying the
   * session into the chat store first. Mirrors `CommandPalette`'s own
   * handling of this id kind. Returns whether the jump happened, so a caller
   * can show "not found" for a session that already rotated out.
   */
  const jumpToConversation = async (sessionId: string): Promise<boolean> => {
    await useConversationsStore.getState().fetch();
    const session = useConversationsStore
      .getState()
      .sessions.find((s) => s.session_id === sessionId);
    if (!session) return false;
    const ok = await useConversationsStore.getState().resume(session);
    if (ok) {
      minimize();
      navigate('/chat');
    }
    return ok;
  };

  return { jumpToAgent, jumpToTask, jumpToConversation };
}
