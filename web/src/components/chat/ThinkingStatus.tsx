import { useState } from 'react';
import { useIntl } from 'react-intl';
import { latestThinkingLine, thinkingTokens, type StepNode, type TurnThinking } from '@/stores/chat-store';
import { ThinkingOrbIndicator } from './ThinkingOrbIndicator';
import { formatThinkingDuration, formatThinkingTokens, useElapsedSeconds } from './thinking-format';

/** The newest still-running tool step, as a one-line fallback progress text. */
function runningStepLine(tree: readonly StepNode[]): string | undefined {
  for (let i = tree.length - 1; i >= 0; i -= 1) {
    const node = tree[i];
    if (node.running) return node.summary ? `${node.tool} · ${node.summary}` : node.tool;
  }
  return undefined;
}

/**
 * ThinkingStatus — the live "the employee is thinking" row shown while a reply
 * is on its way (replaces the bare typing glyph). The counter starts when the
 * user sends, so it works for every runtime, including ones whose provider
 * reports no thinking at all. Below it: approximate tokens when reported, and
 * one progress line (the newest masked `line`, else the running tool step).
 *
 * Accessibility: a polite status region whose only announced text is a fixed
 * "thinking" label; the ticking seconds and the progress line are
 * `aria-hidden` so a screen reader is not interrupted every second. The fade
 * classes are gated behind `prefers-reduced-motion` in `index.css`, and the
 * orb has its own reduced-motion fallback; the counter keeps ticking either way.
 */
export function ThinkingStatus({
  thinking,
  stepTree,
}: {
  thinking: TurnThinking | null;
  stepTree: readonly StepNode[];
}) {
  const intl = useIntl();
  // A turn normally carries its send time; fall back to first render.
  const [mountedAt] = useState(() => Date.now());
  const seconds = useElapsedSeconds(thinking?.startedAt ?? mountedAt);
  const tokens = thinking ? thinkingTokens(thinking) : undefined;
  const line = latestThinkingLine(thinking) ?? runningStepLine(stepTree);

  return (
    <div className="flex justify-start">
      <div
        role="status"
        aria-live="polite"
        className="flex min-w-0 max-w-[80%] items-start gap-2.5 rounded-2xl border border-surface-border bg-surface px-4 py-3 animate-in fade-in-0 duration-200"
      >
        <ThinkingOrbIndicator state="inline" decorative className="mt-0.5 shrink-0" />
        <span className="sr-only">
          {intl.formatMessage({ id: 'chat.thinking.waiting', defaultMessage: 'Thinking…' })}
        </span>
        <div aria-hidden="true" className="min-w-0" data-testid="thinking-status-visual">
          <div className="flex flex-wrap items-baseline gap-x-2 text-sm text-foreground">
            <span className="tabular-nums" data-testid="thinking-status-seconds">
              {formatThinkingDuration(intl, seconds, true)}
            </span>
            {tokens !== undefined && (
              <span className="font-mono text-xs tabular-nums text-muted-foreground">
                {formatThinkingTokens(intl, tokens)}
              </span>
            )}
          </div>
          {line && (
            <p
              key={line}
              className="mt-0.5 truncate text-xs text-muted-foreground animate-in fade-in-0 duration-150"
              data-testid="thinking-status-line"
            >
              {line}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
