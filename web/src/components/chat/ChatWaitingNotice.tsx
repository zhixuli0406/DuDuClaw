import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useChatStore } from '@/stores/chat-store';
import { watchUnansweredReply } from '@/stores/chat-history-poller';

/** Mounted once below the transcript; never touches the composer draft. */
export function ChatWaitingNotice() {
  const intl = useIntl();
  const { sessionId, convId, messages, resumedExplicitly, isStreaming, activeMessageId, lastTurnFrameAt } = useChatStore();
  const last = messages.at(-1);
  const unanswered = resumedExplicitly && !isStreaming && !activeMessageId && last?.role === 'user';
  const key = unanswered ? `${sessionId}:${convId}:${last.id}` : null;
  const [expiredKey, setExpiredKey] = useState<string | null>(null);
  const [clock, setClock] = useState(() => Date.now());
  useEffect(() => {
    if (!key) return;
    return watchUnansweredReply(() => setExpiredKey(key));
  }, [key]);
  useEffect(() => {
    if (!isStreaming || lastTurnFrameAt === null) return;
    // Align to the last real frame, rather than the component's mount time.
    const tick = () => setClock(Date.now());
    const elapsed = Math.max(0, Date.now() - lastTurnFrameAt);
    tick();
    let interval: ReturnType<typeof setInterval> | undefined;
    const timer = setTimeout(() => {
      tick();
      interval = setInterval(tick, 60_000);
    }, 60_000 - elapsed % 60_000);
    return () => { clearTimeout(timer); clearInterval(interval); };
  }, [isStreaming, lastTurnFrameAt]);
  const minutes = isStreaming && lastTurnFrameAt !== null
    ? Math.floor(Math.max(0, clock - lastTurnFrameAt) / 60_000) : 0;
  const id = key ? (key === expiredKey ? 'chat.unansweredExpired' : 'chat.unanswered')
    : minutes >= 1 ? 'chat.stalled' : null;
  if (!id) return null;
  return <p className="px-2 text-sm text-muted-foreground" role="status" aria-live="polite">
    {intl.formatMessage({ id }, { minutes })}
  </p>;
}
