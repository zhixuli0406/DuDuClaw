import { useCallback, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useIntl } from 'react-intl';
import { ChevronLeft, ChevronRight, X } from 'lucide-react';
import { cn } from '@/lib/utils';
import { knowledgeCitationsApi, type KnowledgePassage } from '@/lib/knowledge-citations';

type LoadState =
  | { kind: 'loading' }
  | { kind: 'ready'; passage: KnowledgePassage }
  | { kind: 'denied' }
  | { kind: 'error' };

function isDenied(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err ?? '');
  return /permission denied/i.test(msg);
}

const FOCUSABLE = 'button:not([disabled]), [href], [tabindex]:not([tabindex="-1"])';

/**
 * Side drawer showing one knowledge passage (server-cut before/match/after).
 * Dialog semantics: focus moves in, Tab is trapped, Escape closes and focus
 * returns to the element that opened it. All text is rendered as plain text.
 */
export function CitationPassageDrawer({
  chunkId,
  returnFocusTo,
  onClose,
}: {
  chunkId: number;
  returnFocusTo: HTMLElement | null;
  onClose: () => void;
}) {
  const intl = useIntl();
  const [current, setCurrent] = useState(chunkId);
  const [state, setState] = useState<LoadState>({ kind: 'loading' });
  const panelRef = useRef<HTMLDivElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => setCurrent(chunkId), [chunkId]);

  useEffect(() => {
    let cancelled = false;
    setState({ kind: 'loading' });
    knowledgeCitationsApi
      .passageGet(current)
      .then((passage) => {
        if (!cancelled) setState({ kind: 'ready', passage });
      })
      .catch((err: unknown) => {
        if (!cancelled) setState({ kind: isDenied(err) ? 'denied' : 'error' });
      });
    return () => {
      cancelled = true;
    };
  }, [current]);

  // Focus in on open, hand focus back on close.
  useEffect(() => {
    closeRef.current?.focus();
    return () => {
      returnFocusTo?.focus?.();
    };
  }, [returnFocusTo]);

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== 'Tab') return;
      const nodes = panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE);
      if (!nodes || nodes.length === 0) return;
      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    },
    [onClose],
  );

  const title = intl.formatMessage({ id: 'webchat.citation.drawerTitle', defaultMessage: '來源原文' });
  const passage = state.kind === 'ready' ? state.passage : null;
  const trail = passage
    ? [passage.segment_label, ...(passage.heading_path ?? [])].filter(Boolean).join(' › ')
    : '';

  const navBtn = cn(
    'inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs font-medium',
    'ring-1 ring-inset ring-surface-border hover:bg-muted/60 disabled:opacity-40 disabled:hover:bg-transparent',
    'outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
  );

  return createPortal(
    <div className="fixed inset-0 z-[100]" onKeyDown={onKeyDown}>
      <div className="absolute inset-0 bg-black/30" aria-hidden="true" onClick={onClose} />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className={cn(
          'absolute inset-y-0 right-0 flex w-full max-w-md flex-col bg-surface text-surface-foreground',
          'shadow-xl ring-1 ring-surface-border',
          'motion-safe:transition-transform motion-safe:duration-200',
        )}
      >
        <div className="flex items-start gap-2 border-b border-surface-border px-4 py-3">
          <div className="min-w-0 flex-1">
            <h2 className="break-words text-sm font-semibold">{passage?.title || title}</h2>
            {passage?.locator_text && (
              <p className="mt-0.5 break-words text-xs text-muted-foreground">{passage.locator_text}</p>
            )}
            {trail && <p className="mt-0.5 break-words text-xs text-muted-foreground">{trail}</p>}
          </div>
          <button
            ref={closeRef}
            type="button"
            onClick={onClose}
            aria-label={intl.formatMessage({ id: 'webchat.citation.close', defaultMessage: '關閉' })}
            className="rounded-md p-1.5 hover:bg-muted/60 outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <X className="size-4" aria-hidden="true" />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3 text-sm leading-relaxed" aria-live="polite">
          {state.kind === 'loading' && (
            <p className="text-muted-foreground">
              {intl.formatMessage({ id: 'webchat.citation.loading', defaultMessage: '載入中…' })}
            </p>
          )}
          {state.kind === 'denied' && (
            <p role="alert">
              {intl.formatMessage({
                id: 'webchat.citation.passageDenied',
                defaultMessage: '你目前沒有權限查看這段內容',
              })}
            </p>
          )}
          {state.kind === 'error' && (
            <p role="alert">
              {intl.formatMessage({
                id: 'webchat.citation.passageError',
                defaultMessage: '暫時無法載入這段內容，請稍後再試',
              })}
            </p>
          )}
          {passage && (
            <>
              {passage.suspect && (
                <p
                  role="note"
                  className="mb-3 rounded-md bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-300"
                >
                  {intl.formatMessage({
                    id: 'webchat.citation.suspect',
                    defaultMessage: '這段文字疑似含有指令，只供參考',
                  })}
                </p>
              )}
              <p className="whitespace-pre-wrap break-words">
                <span className="text-muted-foreground" data-passage-part="before">
                  {passage.before}
                </span>
                <mark className="rounded bg-amber-300/40 px-0.5 text-inherit" data-passage-part="match">
                  {passage.match}
                </mark>
                <span className="text-muted-foreground" data-passage-part="after">
                  {passage.after}
                </span>
              </p>
            </>
          )}
        </div>

        <div className="flex items-center justify-between gap-2 border-t border-surface-border px-4 py-3">
          <button
            type="button"
            className={navBtn}
            disabled={passage?.prev_chunk_id == null}
            onClick={() => passage?.prev_chunk_id != null && setCurrent(passage.prev_chunk_id)}
          >
            <ChevronLeft className="size-3.5" aria-hidden="true" />
            {intl.formatMessage({ id: 'webchat.citation.prev', defaultMessage: '上一段' })}
          </button>
          <button
            type="button"
            className={navBtn}
            disabled={passage?.next_chunk_id == null}
            onClick={() => passage?.next_chunk_id != null && setCurrent(passage.next_chunk_id)}
          >
            {intl.formatMessage({ id: 'webchat.citation.next', defaultMessage: '下一段' })}
            <ChevronRight className="size-3.5" aria-hidden="true" />
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
