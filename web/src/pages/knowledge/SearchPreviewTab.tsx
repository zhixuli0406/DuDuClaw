import { useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, Loader2, Search } from 'lucide-react';
import { Badge, Button, Empty, ErrorState, Input } from '@/components/mds';
import { PassageDrawer } from '@/components/knowledge/PassageDrawer';
import { knowledgeApi, whereText, type KbCollection, type KbSearchResult } from '@/lib/knowledge-api';

/** Characters of each passage shown in the result list. */
export const SNIPPET_CHARS = 220;

/** First `n` characters (code points, not UTF-16 units) plus an ellipsis. */
export function snippet(text: string, n = SNIPPET_CHARS): string {
  const chars = Array.from(text.replace(/\s+/g, ' ').trim());
  return chars.length > n ? `${chars.slice(0, n).join('')}…` : chars.join('');
}

/**
 * 試搜尋 — ask a question the way an AI employee would and see which
 * passages of this collection come back, in rank order, with where each
 * one sits in its document. Click a result to read it in context.
 */
export function SearchPreviewTab({ collection }: { collection: KbCollection }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [query, setQuery] = useState('');
  const [result, setResult] = useState<KbSearchResult | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [openChunk, setOpenChunk] = useState<number | null>(null);

  const run = async (e?: FormEvent) => {
    e?.preventDefault();
    const q = query.trim();
    if (!q || loading) return;
    setLoading(true);
    setError(null);
    try {
      setResult(await knowledgeApi.search(q, [collection.id], 10));
    } catch (err) {
      setError(err);
      setResult(null);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="space-y-4">
      <form onSubmit={(e) => void run(e)} className="flex gap-2" role="search">
        <div className="relative flex-1">
          <Search aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('kb.search.placeholder')}
            aria-label={t('kb.search.placeholder')}
            className="pl-8"
            maxLength={500}
          />
        </div>
        <Button type="submit" variant="brand" disabled={loading || query.trim() === ''}>
          {loading ? <Loader2 aria-hidden className="motion-safe:animate-spin" /> : <Search aria-hidden />}
          {t('kb.search.run')}
        </Button>
      </form>

      {error != null ? (
        <ErrorState variant="inline" error={error} onRetry={() => void run()} />
      ) : result === null ? (
        <p className="text-sm text-muted-foreground">{t('kb.search.intro')}</p>
      ) : (
        <div className="space-y-3" aria-live="polite">
          <div className="flex flex-wrap items-center gap-2">
            <Badge variant="outline">{t(`kb.search.mode.${result.mode}`)}</Badge>
            <span className="text-xs text-muted-foreground">{t('kb.search.count', { count: result.hits.length })}</span>
          </div>
          {result.vector_unavailable && result.mode === 'keyword_only' && (
            <p className="text-xs text-muted-foreground">{t('kb.search.keywordNote')}</p>
          )}
          {result.hits.length === 0 ? (
            <Empty icon={Search} title={t('kb.search.none')} description={t('kb.search.noneDesc')} />
          ) : (
            <ol className="space-y-2">
              {result.hits.map((h, i) => (
                <li key={h.chunk_id}>
                  <button
                    type="button"
                    onClick={() => setOpenChunk(h.chunk_id)}
                    className="w-full rounded-xl border border-surface-border px-3 py-2.5 text-left transition-colors outline-none hover:bg-surface-hover focus-visible:ring-3 focus-visible:ring-ring/50 sm:px-4"
                  >
                    <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                      <span className="font-mono text-xs tabular-nums text-muted-foreground">[{i + 1}]</span>
                      <span className="min-w-0 truncate text-sm font-medium">{h.title}</span>
                      <span className="text-xs text-muted-foreground">
                        {whereText(h.locator_text, h.heading_path)}
                      </span>
                      {h.suspect && (
                        <span className="inline-flex items-center gap-1 text-xs text-amber-700 dark:text-amber-400">
                          <AlertTriangle aria-hidden className="size-3" />
                          {t('kb.search.suspect')}
                        </span>
                      )}
                    </div>
                    <p className="mt-1 text-sm text-muted-foreground">{snippet(h.text)}</p>
                  </button>
                </li>
              ))}
            </ol>
          )}
        </div>
      )}
      <PassageDrawer chunkId={openChunk} onClose={() => setOpenChunk(null)} />
    </div>
  );
}
