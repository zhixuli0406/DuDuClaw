import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, FileText, Globe, Loader2, MoreHorizontal, Plus, RefreshCw, Trash2, Type } from 'lucide-react';
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  Empty,
  ErrorState,
} from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { SourceStatusChip } from '@/components/knowledge/SourceStatusChip';
import { toast, formatError } from '@/lib/toast';
import { downloadOriginal, isSourceBusy, knowledgeApi, type KbCollection, type KbSource } from '@/lib/knowledge-api';
import { AddSourceDialog } from './AddSourceDialog';
import { formatBytes, formatWhen } from './labels';

/** How often the list refreshes while a source is still being read. */
export const POLL_MS = 3000;

const KIND_ICON = { file: FileText, paste: Type, url: Globe, attachment: FileText } as const;

/**
 * 來源 — every document in the collection with its reading state. While a
 * source is still being read the list refreshes itself; a failed source
 * shows the reason in plain words, with 重新讀取 and 刪除 next to it.
 */
export function SourcesTab({
  collection,
  canEdit,
  onChanged,
}: {
  collection: KbCollection;
  canEdit: boolean;
  onChanged: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [sources, setSources] = useState<KbSource[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);
  const [toDelete, setToDelete] = useState<KbSource | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;

  const load = useCallback(async () => {
    try {
      const r = await knowledgeApi.listSources(collection.id);
      setSources(r.sources);
      setError(null);
    } catch (e) {
      setError(e);
    }
  }, [collection.id]);

  useEffect(() => {
    void load();
  }, [load]);

  const anyBusy = (sources ?? []).some((s) => isSourceBusy(s.status));
  useEffect(() => {
    if (!anyBusy) return;
    const h = setInterval(() => {
      if (typeof document === 'undefined' || document.visibilityState !== 'hidden') void load();
    }, POLL_MS);
    return () => clearInterval(h);
  }, [anyBusy, load]);

  const reindex = async (s: KbSource) => {
    setBusyId(s.id);
    try {
      await knowledgeApi.reindexSource(s.id);
      toast.success(t('kb.source.reindexQueued', { title: s.title }));
      await load();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusyId(null);
    }
  };

  const remove = async (s: KbSource) => {
    setBusyId(s.id);
    try {
      await knowledgeApi.deleteSource(s.id);
      toast.success(t('kb.source.deleted', { title: s.title }));
      setToDelete(null);
      await load();
      onChangedRef.current();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusyId(null);
    }
  };

  const download = async (s: KbSource) => {
    try {
      await downloadOriginal(s);
    } catch (e) {
      toast.error(formatError(e));
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm text-muted-foreground">{t('kb.sources.intro')}</p>
        {canEdit && (
          <Button variant="brand" size="sm" onClick={() => setAdding(true)}>
            <Plus aria-hidden />
            {t('kb.source.add')}
          </Button>
        )}
      </div>

      {sources === null && error != null ? (
        <ErrorState error={error} onRetry={() => void load()} />
      ) : sources === null ? (
        <div className="flex justify-center py-10">
          <Loader2 aria-hidden className="size-5 text-muted-foreground motion-safe:animate-spin" />
          <span className="sr-only">{t('common.loading')}</span>
        </div>
      ) : sources.length === 0 ? (
        <Empty
          icon={FileText}
          title={t('kb.sources.empty')}
          description={canEdit ? t('kb.sources.emptyEdit') : t('kb.sources.emptyView')}
          action={
            canEdit ? (
              <Button variant="outline" size="sm" onClick={() => setAdding(true)}>
                <Plus aria-hidden />
                {t('kb.source.add')}
              </Button>
            ) : undefined
          }
        />
      ) : (
        <ul className="divide-y divide-surface-border overflow-hidden rounded-xl border border-surface-border" aria-live="polite">
          {sources.map((s) => {
            const Icon = KIND_ICON[s.kind] ?? FileText;
            return (
              <li key={s.id} className="flex items-start gap-3 px-3 py-3 sm:px-4" data-testid="kb-source-row">
                <Icon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1 space-y-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="min-w-0 truncate text-sm font-medium" title={s.title}>
                      {s.title}
                    </span>
                    <SourceStatusChip source={s} />
                  </div>
                  <p className="text-xs text-muted-foreground">
                    <span className="uppercase">{s.format}</span>
                    {s.bytes > 0 && ` · ${formatBytes(s.bytes)}`}
                    {s.pages ? ` · ${t('kb.source.pages', { count: s.pages })}` : ''}
                    {s.status === 'ready' && ` · ${t('kb.source.passages', { count: s.chunk_count })}`}
                    {` · ${formatWhen(intl, s.created_at)}`}
                  </p>
                  {s.status === 'failed' && s.error_message && (
                    <p className="text-xs text-destructive" role="status">
                      {s.error_message}
                    </p>
                  )}
                </div>
                <DropdownMenu>
                  <DropdownMenuTrigger
                    render={
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        disabled={busyId === s.id}
                        aria-label={t('kb.source.actions', { title: s.title })}
                      />
                    }
                  >
                    <MoreHorizontal />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent>
                    {s.kind !== 'url' && (
                      <DropdownMenuItem onClick={() => void download(s)}>
                        <Download />
                        {t('kb.source.download')}
                      </DropdownMenuItem>
                    )}
                    {canEdit && (
                      <DropdownMenuItem disabled={isSourceBusy(s.status)} onClick={() => void reindex(s)}>
                        <RefreshCw />
                        {t('kb.source.reindex')}
                      </DropdownMenuItem>
                    )}
                    {canEdit && (
                      <DropdownMenuItem variant="destructive" onClick={() => setToDelete(s)}>
                        <Trash2 />
                        {t('common.delete')}
                      </DropdownMenuItem>
                    )}
                  </DropdownMenuContent>
                </DropdownMenu>
              </li>
            );
          })}
        </ul>
      )}

      {adding && (
        <AddSourceDialog
          collection={collection}
          onClose={() => setAdding(false)}
          onAdded={() => {
            void load();
            onChangedRef.current();
          }}
        />
      )}
      {toDelete && (
        <ConfirmDialog
          open
          title={t('kb.source.deleteTitle')}
          message={t('kb.source.deleteConfirm', { title: toDelete.title })}
          confirmLabel={t('common.delete')}
          busy={busyId === toDelete.id}
          onConfirm={() => void remove(toDelete)}
          onClose={() => setToDelete(null)}
        />
      )}
    </div>
  );
}
