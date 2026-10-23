import { Fragment, useMemo, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { splitCitationMarkers, type ChatCitation } from '@/lib/knowledge-citations';

/** Open request: which passage, and the element to hand focus back to. */
export type OpenCitation = (chunkId: number, trigger: HTMLElement | null) => void;

function citationLabelParts(c: ChatCitation): string {
  return [c.title, c.locator].filter(Boolean).join(' ');
}

/** True when the citation can open a passage for the viewer. */
export function isCitationOpenable(c: ChatCitation): boolean {
  return c.state === 'available' && c.chunk_id != null;
}

/**
 * Message text with `[n]` markers (that match a citation) rendered as small
 * superscript buttons. Everything else, including markers without a matching
 * citation and anything inside code, stays plain text.
 */
export function CitedText({
  text,
  citations,
  onOpen,
}: {
  text: string;
  citations: readonly ChatCitation[];
  onOpen: OpenCitation;
}) {
  const intl = useIntl();
  const byN = useMemo(() => new Map(citations.map((c) => [c.n, c])), [citations]);
  const segments = useMemo(() => splitCitationMarkers(text, new Set(byN.keys())), [text, byN]);
  return (
    <>
      {segments.map((seg, i): ReactNode => {
        if (seg.kind === 'text') return <Fragment key={i}>{seg.text}</Fragment>;
        const c = byN.get(seg.n)!;
        const openable = isCitationOpenable(c);
        const label = intl.formatMessage(
          { id: 'webchat.citation.marker', defaultMessage: '來源 {n}：{label}' },
          { n: c.n, label: citationLabelParts(c) || String(c.n) },
        );
        return (
          <sup key={i} className="mx-px align-baseline">
            <button
              type="button"
              data-citation-marker={c.n}
              aria-label={label}
              title={label}
              disabled={!openable}
              onClick={(e) => openable && onOpen(c.chunk_id as number, e.currentTarget)}
              className={cn(
                'rounded px-1 text-[0.7rem] font-medium tabular-nums leading-none',
                'bg-primary/10 text-primary',
                openable ? 'cursor-pointer hover:bg-primary/20' : 'cursor-default opacity-60',
                'outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
              )}
            >
              {seg.raw}
            </button>
          </sup>
        );
      })}
    </>
  );
}

/** Compact 「來源」 list below an assistant message. */
export function CitationSourceList({
  citations,
  onOpen,
}: {
  citations: readonly ChatCitation[];
  onOpen: OpenCitation;
}) {
  const intl = useIntl();
  const sorted = useMemo(() => [...citations].sort((a, b) => a.n - b.n), [citations]);
  if (sorted.length === 0) return null;
  return (
    <section
      className="mt-2 border-t border-surface-border pt-2"
      aria-label={intl.formatMessage({ id: 'webchat.citation.heading', defaultMessage: '來源' })}
    >
      <h4 className="mb-1 text-xs font-medium text-muted-foreground">
        {intl.formatMessage({ id: 'webchat.citation.heading', defaultMessage: '來源' })}
      </h4>
      <ul className="space-y-1">
        {sorted.map((c) => {
          const openable = isCitationOpenable(c);
          const head = [c.title, c.locator].filter(Boolean).join(' · ');
          const body = (
            <>
              <span className="flex flex-wrap items-baseline gap-x-1.5 text-xs">
                <span className="font-medium tabular-nums">[{c.n}]</span>
                {c.state === 'restricted' ? (
                  <span className="text-muted-foreground">
                    {intl.formatMessage({
                      id: 'webchat.citation.restricted',
                      defaultMessage: '你目前沒有權限查看這個來源',
                    })}
                  </span>
                ) : (
                  <>
                    <span className="break-words">{head}</span>
                    {c.state === 'deleted' && (
                      <span className="rounded bg-muted px-1.5 py-0.5 text-[0.7rem] text-muted-foreground">
                        {intl.formatMessage({
                          id: 'webchat.citation.deleted',
                          defaultMessage: '來源已刪除',
                        })}
                      </span>
                    )}
                    {openable && (
                      <span className="text-primary">
                        {intl.formatMessage({
                          id: 'webchat.citation.viewOriginal',
                          defaultMessage: '查看原文',
                        })}
                      </span>
                    )}
                  </>
                )}
              </span>
              {c.state === 'available' && c.excerpt && (
                <span className="mt-0.5 line-clamp-2 block break-words text-xs text-muted-foreground">
                  {c.excerpt}
                </span>
              )}
            </>
          );
          return (
            <li key={c.n} data-citation-row={c.n}>
              {openable ? (
                <button
                  type="button"
                  onClick={(e) => onOpen(c.chunk_id as number, e.currentTarget)}
                  className={cn(
                    'block w-full rounded-md px-1.5 py-1 text-left hover:bg-muted/60',
                    'outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
                  )}
                >
                  {body}
                </button>
              ) : (
                <div className="px-1.5 py-1">{body}</div>
              )}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
