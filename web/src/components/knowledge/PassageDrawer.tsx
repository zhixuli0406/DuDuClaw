import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ChevronLeft, ChevronRight, Loader2 } from 'lucide-react';
import {
  Button,
  ErrorState,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/mds';
import { knowledgeApi, whereText, type KbPassage } from '@/lib/knowledge-api';

/**
 * PassageDrawer — the original text around one passage, with the passage
 * itself highlighted. The gateway re-checks permission on every open, so a
 * passage that became off-limits shows an error instead of text. Prev/next
 * walk the neighbouring passages of the same source.
 */
export function PassageDrawer({ chunkId, onClose }: { chunkId: number | null; onClose: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [current, setCurrent] = useState<number | null>(chunkId);
  const [passage, setPassage] = useState<KbPassage | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const markRef = useRef<HTMLElement>(null);

  useEffect(() => setCurrent(chunkId), [chunkId]);

  useEffect(() => {
    if (current === null) return;
    let alive = true;
    setLoading(true);
    setError(null);
    knowledgeApi
      .getPassage(current)
      .then((p) => alive && setPassage(p))
      .catch((e: unknown) => {
        if (!alive) return;
        setPassage(null);
        setError(e);
      })
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [current]);

  // Bring the passage itself into view; the context above it can be long.
  useEffect(() => {
    if (passage) markRef.current?.scrollIntoView?.({ block: 'center' });
  }, [passage]);

  const open = chunkId !== null;
  return (
    <Sheet open={open} onOpenChange={(o) => !o && onClose()}>
      <SheetContent side="right" className="w-full max-w-full sm:max-w-xl">
        <SheetHeader>
          <SheetTitle className="pr-8">{passage?.title ?? t('kb.passage.title')}</SheetTitle>
          <SheetDescription>
            {passage ? whereText(passage.locator_text, passage.heading_path) : ' '}
          </SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-y-auto" aria-live="polite" aria-busy={loading}>
          {loading && !passage ? (
            <div className="flex justify-center py-10">
              <Loader2 aria-hidden className="size-5 text-muted-foreground motion-safe:animate-spin" />
              <span className="sr-only">{t('common.loading')}</span>
            </div>
          ) : error != null ? (
            <ErrorState variant="inline" title={t('kb.passage.loadError')} error={error} />
          ) : passage ? (
            <article className="space-y-3">
              {passage.suspect && (
                <p className="flex items-start gap-2 rounded-md bg-amber-500/12 px-3 py-2 text-xs text-amber-700 dark:text-amber-400">
                  <AlertTriangle aria-hidden className="mt-0.5 size-3.5 shrink-0" />
                  {t('kb.passage.suspect')}
                </p>
              )}
              {passage.segment_label && (
                <p className="text-xs text-muted-foreground">{passage.segment_label}</p>
              )}
              <p className="whitespace-pre-wrap break-words text-sm leading-relaxed text-foreground">
                {passage.before && <span className="text-muted-foreground">{passage.before}</span>}
                <mark ref={markRef} className="rounded-sm bg-amber-500/20 px-0.5 text-foreground">{passage.match}</mark>
                {passage.after && <span className="text-muted-foreground">{passage.after}</span>}
              </p>
            </article>
          ) : null}
        </div>
        <div className="flex items-center justify-between gap-2 border-t border-surface-border pt-3">
          <Button
            variant="outline"
            size="sm"
            disabled={!passage?.prev_chunk_id || loading}
            onClick={() => passage?.prev_chunk_id && setCurrent(passage.prev_chunk_id)}
          >
            <ChevronLeft aria-hidden />
            {t('kb.passage.prev')}
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={!passage?.next_chunk_id || loading}
            onClick={() => passage?.next_chunk_id && setCurrent(passage.next_chunk_id)}
          >
            {t('kb.passage.next')}
            <ChevronRight aria-hidden />
          </Button>
        </div>
      </SheetContent>
    </Sheet>
  );
}
