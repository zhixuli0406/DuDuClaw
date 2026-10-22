import { useEffect, useState } from 'react';
import type { IntlShape } from 'react-intl';
import { formatApproxTokens, thinkingSeconds } from '@/stores/chat-thinking';

/**
 * "思考中… 12 秒" / "思考了 1 分 5 秒" — one formatter for the live counter and
 * the collapsed line so both read the same number for the same span.
 */
export function formatThinkingDuration(intl: IntlShape, seconds: number, live: boolean): string {
  if (seconds >= 60) {
    const values = { minutes: Math.floor(seconds / 60), seconds: seconds % 60 };
    return live
      ? intl.formatMessage({ id: 'chat.thinking.liveMinutes', defaultMessage: 'Thinking… {minutes}m {seconds}s' }, values)
      : intl.formatMessage({ id: 'chat.thinking.doneMinutes', defaultMessage: 'Thought for {minutes}m {seconds}s' }, values);
  }
  return live
    ? intl.formatMessage({ id: 'chat.thinking.live', defaultMessage: 'Thinking… {seconds}s' }, { seconds })
    : intl.formatMessage({ id: 'chat.thinking.done', defaultMessage: 'Thought for {seconds}s' }, { seconds });
}

/** "約 1.2k tokens". */
export function formatThinkingTokens(intl: IntlShape, tokens: number): string {
  return intl.formatMessage(
    { id: 'chat.thinking.tokens', defaultMessage: '~{tokens} tokens' },
    { tokens: formatApproxTokens(tokens) },
  );
}

/**
 * Whole seconds since `startedAt`, re-rendering on each second boundary while
 * `endedAt` is unset. With `endedAt` the value is fixed (no timer). The
 * number follows `thinkingSeconds`, so the collapsed line matches the last
 * live value. Timers run regardless of reduced motion — a counter is
 * information, not animation.
 */
export function useElapsedSeconds(startedAt: number, endedAt?: number): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (endedAt !== undefined) return undefined;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const schedule = () => {
      const elapsed = Math.max(0, Date.now() - startedAt);
      // Wake just after the next whole-second boundary of this span.
      const delay = 1000 - (elapsed % 1000) + 5;
      timer = setTimeout(() => {
        setNow(Date.now());
        schedule();
      }, delay);
    };
    schedule();
    return () => {
      if (timer) clearTimeout(timer);
    };
  }, [startedAt, endedAt]);

  return thinkingSeconds(startedAt, endedAt ?? now);
}
