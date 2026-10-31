import { useChatStore } from './chat-store';
import { useConversationsStore } from './conversations-store';

const FAST_WINDOW = 2 * 60_000;
const MAX_WAIT = 30 * 60_000;

/** One mounted view owns one bounded timer and at most one history request.
 * Disposing also fences a request already waiting on the history RPC. */
export function watchUnansweredReply(onExpired: () => void): () => void {
  const initial = useChatStore.getState();
  const lastId = initial.messages.at(-1)?.id;
  const startedAt = Date.now();
  let disposed = false;
  let expired = false;
  let inFlight = false;
  let refreshAgain: boolean | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const current = () => {
    const now = useChatStore.getState();
    return !disposed && now.sessionId === initial.sessionId && now.convId === initial.convId
      && now.resumedExplicitly && !now.isStreaming && !now.activeMessageId
      && now.messages.at(-1)?.id === lastId && now.messages.at(-1)?.role === 'user';
  };
  const refresh = async (allowExpired = false) => {
    if (!current() || (expired && !allowExpired)) return;
    if (inFlight) { refreshAgain = (refreshAgain ?? false) || allowExpired; return; }
    inFlight = true;
    try { await useConversationsStore.getState().refreshUnanswered(() => current() && (allowExpired || !expired)); }
    finally {
      inFlight = false;
      if (refreshAgain !== null) { const again = refreshAgain; refreshAgain = null; void refresh(again); }
    }
  };
  const schedule = () => {
    const elapsed = Date.now() - startedAt;
    timer = setTimeout(() => {
      if (!current()) return;
      if (Date.now() - startedAt >= MAX_WAIT) {
        expired = true;
        onExpired();
        return;
      }
      void refresh();
      schedule();
    }, Math.min(elapsed < FAST_WINDOW ? 5_000 : 30_000, MAX_WAIT - elapsed));
  };
  const unsubscribe = useChatStore.subscribe((next, previous) => {
    if (!current()) {
      clearTimeout(timer);
      return;
    }
    // A completed turn may carry the old conv nonce after history was reopened.
    // Keep this one-shot refresh available even after automatic polling ends.
    if (next.historyReplyRevision !== previous.historyReplyRevision) void refresh(true);
  });
  if (current() && !expired) schedule();
  return () => { disposed = true; clearTimeout(timer); unsubscribe(); };
}
