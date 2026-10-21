import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Loader2 } from 'lucide-react';
import {
  Button,
  ErrorState,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/mds';
import { knowledgeApi, type KbDeniedRow } from '@/lib/knowledge-api';
import { formatWhen } from './labels';
import { requesterText } from './AccessLogTab';
import { principalLabels } from '@/components/knowledge/PrincipalPicker';

/**
 * Admin only: every read the knowledge base refused, across all
 * collections, newest first. A refused read means someone (or an AI
 * employee acting for someone) asked for a passage they were not shared on.
 */
export function DeniedReadsSheet({ open, onClose }: { open: boolean; onClose: () => void }) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [rows, setRows] = useState<KbDeniedRow[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [more, setMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [labels, setLabels] = useState<Map<string, string>>(new Map());

  const load = useCallback(async (before?: number) => {
    setLoading(true);
    try {
      setError(null);
      const r = await knowledgeApi.deniedReads(before);
      setRows((prev) => (before === undefined ? r.entries : [...(prev ?? []), ...r.entries]));
      setMore(r.entries.length >= 50);
    } catch (e) {
      setError(e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    void load();
    knowledgeApi
      .searchPrincipals('', '')
      .then((p) => setLabels(principalLabels(p)))
      .catch(() => {
        /* Names are a nicety; ids still show. */
      });
  }, [open, load]);

  return (
    <Sheet open={open} onOpenChange={(o) => !o && onClose()}>
      <SheetContent side="right" className="w-full max-w-full sm:max-w-lg">
        <SheetHeader>
          <SheetTitle className="pr-8">{t('kb.denied.title')}</SheetTitle>
          <SheetDescription>{t('kb.denied.desc')}</SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-y-auto" aria-busy={loading}>
          {error != null && rows === null ? (
            <ErrorState variant="inline" error={error} onRetry={() => void load()} />
          ) : rows === null ? (
            <div className="flex justify-center py-8">
              <Loader2 aria-hidden className="size-5 text-muted-foreground motion-safe:animate-spin" />
              <span className="sr-only">{t('common.loading')}</span>
            </div>
          ) : rows.length === 0 ? (
            <p className="py-8 text-center text-sm text-muted-foreground">{t('kb.denied.empty')}</p>
          ) : (
            <ul className="divide-y divide-surface-border">
              {rows.map((r) => (
                <li key={r.id} className="space-y-0.5 py-2.5">
                  <p className="text-sm">
                    <span className="font-medium">{requesterText(intl, r, labels)}</span>
                    {' · '}
                    {t(`kb.op.${r.op}`)}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {formatWhen(intl, r.at)}
                    {r.collection_name ? ` · ${r.collection_name}` : ''}
                    {r.query ? ` · 「${r.query}」` : ''}
                  </p>
                </li>
              ))}
            </ul>
          )}
          {more && (
            <div className="flex justify-center pt-3">
              <Button variant="outline" size="sm" disabled={loading} onClick={() => void load(rows?.[rows.length - 1]?.id)}>
                {t('kb.log.more')}
              </Button>
            </div>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
