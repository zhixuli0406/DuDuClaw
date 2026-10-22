import { useId, useState } from 'react';
import { useIntl, type IntlShape } from 'react-intl';
import { ChevronDown, Lightbulb } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  blocksWithText,
  thinkingSeconds,
  thinkingTokens,
  type ThinkingBlock,
  type TurnThinking,
} from '@/stores/chat-store';
import { formatThinkingDuration, formatThinkingTokens, useElapsedSeconds } from './thinking-format';

function blockLabel(intl: IntlShape, block: ThinkingBlock): string {
  switch (block.kind) {
    case 'raw':
      return intl.formatMessage({ id: 'chat.thinking.rawLabel', defaultMessage: 'Raw reasoning (secrets masked)' });
    case 'progress':
      return intl.formatMessage({ id: 'chat.thinking.progressLabel', defaultMessage: 'Progress' });
    default:
      return intl.formatMessage({ id: 'chat.thinking.summaryLabel', defaultMessage: 'Thinking summary' });
  }
}

/** Fixed notes for what could not be shown (never left blank). */
function turnNotes(intl: IntlShape, thinking: TurnThinking, hasText: boolean): string[] {
  const notes: string[] = [];
  if (thinking.blocks.some((b) => b.kind === 'redacted')) {
    notes.push(intl.formatMessage({ id: 'chat.thinking.redacted', defaultMessage: 'Some thinking was withheld by safety rules' }));
  }
  if (!hasText && thinking.blocks.some((b) => b.kind === 'omitted')) {
    notes.push(
      intl.formatMessage({ id: 'chat.thinking.omitted', defaultMessage: "This model doesn't provide a thinking summary" }),
    );
  }
  if (thinking.rawNeedsRedaction) {
    notes.push(
      intl.formatMessage({
        id: 'chat.thinking.rawNeedsRedaction',
        defaultMessage: 'Enable redaction to show raw reasoning from open models',
      }),
    );
  }
  return notes;
}

function BlockView({ block }: { block: ThinkingBlock }) {
  const intl = useIntl();
  return (
    <div className="space-y-0.5">
      <div className="flex items-baseline gap-2 text-xs font-medium text-muted-foreground">
        <span>{blockLabel(intl, block)}</span>
        {block.durationMs !== undefined && block.durationMs > 0 && (
          <span className="font-mono tabular-nums font-normal">
            {intl.formatMessage(
              { id: 'chat.thinking.blockSeconds', defaultMessage: '{seconds}s' },
              { seconds: thinkingSeconds(0, block.durationMs) },
            )}
          </span>
        )}
      </div>
      <p className="whitespace-pre-wrap break-words text-xs leading-relaxed text-muted-foreground">
        {block.text}
        {block.truncated && (
          <span className="ml-1 text-muted-foreground">
            {intl.formatMessage({ id: 'chat.thinking.truncated', defaultMessage: '(truncated)' })}
          </span>
        )}
      </p>
    </div>
  );
}

/**
 * ThinkingSection — the collapsed "thought for N s · ~1.2k tokens" line of a
 * finished turn. N is the wall time from send to reply (the same number the
 * live counter showed). Only expandable when a block actually has text; a
 * model that reports no text (omitted) or text the safety rules withheld
 * (redacted) gets a fixed note instead of an empty panel.
 *
 * `compact` is the quieter form shown above earlier replies: no inline notes
 * (they move to the tooltip and a screen-reader-only line).
 */
export function ThinkingSection({ thinking, compact = false }: { thinking: TurnThinking; compact?: boolean }) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const seconds = useElapsedSeconds(thinking.startedAt, thinking.endedAt ?? thinking.startedAt);
  const tokens = thinkingTokens(thinking);
  const textBlocks = blocksWithText(thinking);
  const expandable = textBlocks.length > 0;
  const notes = turnNotes(intl, thinking, expandable);

  const summary = (
    <>
      <Lightbulb className="h-3.5 w-3.5 shrink-0 text-muted-foreground/70" aria-hidden="true" />
      <span className="tabular-nums">{formatThinkingDuration(intl, seconds, false)}</span>
      {tokens !== undefined && (
        <>
          <span aria-hidden="true">·</span>
          <span className="font-mono tabular-nums">{formatThinkingTokens(intl, tokens)}</span>
        </>
      )}
    </>
  );
  const rowClass = cn('flex items-center gap-1.5 text-xs text-muted-foreground', compact && 'pl-1');

  return (
    <div className={cn(!compact && 'space-y-1.5')} data-testid="thinking-section">
      {expandable ? (
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          aria-controls={panelId}
          className={cn(rowClass, 'rounded-md hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/50')}
        >
          <ChevronDown
            className={cn('h-3.5 w-3.5 shrink-0 motion-safe:transition-transform', !open && '-rotate-90')}
            aria-hidden="true"
            data-testid="thinking-chevron"
          />
          {summary}
        </button>
      ) : (
        <div className={rowClass} title={compact && notes.length > 0 ? notes.join('\n') : undefined}>
          {summary}
          {compact && notes.length > 0 && <span className="sr-only">{notes.join(' ')}</span>}
        </div>
      )}

      {expandable && open && (
        <div
          id={panelId}
          className="space-y-2 border-l-2 border-border pl-3 animate-in fade-in-0 duration-150"
        >
          {textBlocks.map((b) => (
            <BlockView key={b.id} block={b} />
          ))}
          {notes.map((n) => (
            <p key={n} className="text-xs text-muted-foreground">
              {n}
            </p>
          ))}
        </div>
      )}

      {!expandable && !compact && notes.length > 0 && (
        <div className="space-y-0.5 pl-5">
          {notes.map((n) => (
            <p key={n} className="text-xs text-muted-foreground">
              {n}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
