import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, FileText, Globe, MoreHorizontal, Trash2, Type } from 'lucide-react';
import {
  Button,
  Checkbox,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/mds';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { SourceStatusChip } from '@/components/knowledge/SourceStatusChip';
import { toast, formatError } from '@/lib/toast';
import { downloadOriginal, isSourceBusy, knowledgeApi, type KbSource } from '@/lib/knowledge-api';
import { formatBytes, formatWhen } from '@/pages/knowledge/labels';

const KIND_ICON = { file: FileText, paste: Type, url: Globe, attachment: FileText } as const;

/**
 * The files of an employee's own collection: reading state (with the
 * semantic-analysis percentage while it runs), the failure reason in plain
 * words, download, and — for people who may change them — removal, with
 * the option to have the employee forget what it learned from the file.
 */
export function OwnFiles({
  sources,
  canManage,
  onChanged,
}: {
  sources: KbSource[];
  canManage: boolean;
  onChanged: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [toDelete, setToDelete] = useState<KbSource | null>(null);
  const [forget, setForget] = useState(false);
  const [busy, setBusy] = useState(false);

  const remove = async (s: KbSource) => {
    setBusy(true);
    try {
      const r = await knowledgeApi.deleteSource(s.id, forget);
      const filed = r.forget?.filed?.length ?? 0;
      toast.success(
        forget
          ? t('agentSources.files.removedForget', { title: s.title, count: filed })
          : t('kb.source.deleted', { title: s.title }),
      );
      setToDelete(null);
      onChanged();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
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
    <>
      <ul className="divide-y divide-surface-border overflow-hidden rounded-xl border border-surface-border" aria-live="polite">
        {sources.map((s) => {
          const Icon = KIND_ICON[s.kind] ?? FileText;
          return (
            <li key={s.id} className="flex items-start gap-3 px-3 py-3 sm:px-4" data-testid="agent-own-file">
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
                    <Button variant="ghost" size="icon-sm" disabled={busy} aria-label={t('kb.source.actions', { title: s.title })} />
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
                  {canManage && (
                    <DropdownMenuItem
                      variant="destructive"
                      disabled={s.status === 'deleting'}
                      onClick={() => {
                        setForget(false);
                        setToDelete(s);
                      }}
                    >
                      <Trash2 />
                      {t('agentSources.files.remove')}
                    </DropdownMenuItem>
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
            </li>
          );
        })}
      </ul>
      {toDelete && (
        <ConfirmDialog
          open
          title={t('agentSources.files.removeTitle')}
          message={t('agentSources.files.removeConfirm', { title: toDelete.title })}
          confirmLabel={t('agentSources.files.remove')}
          busy={busy}
          onConfirm={() => void remove(toDelete)}
          onClose={() => setToDelete(null)}
        >
          <label className="flex items-start gap-2 text-sm">
            <Checkbox
              checked={forget}
              onCheckedChange={(v) => setForget(v === true)}
              aria-label={t('agentSources.files.forget')}
            />
            <span>
              {t('agentSources.files.forget')}
              <span className="block text-xs text-muted-foreground">{t('agentSources.files.forgetHint')}</span>
            </span>
          </label>
        </ConfirmDialog>
      )}
    </>
  );
}

/** Whether any file is still being read (the tab keeps refreshing). */
export function anyBusy(sources: readonly KbSource[] | null | undefined): boolean {
  return (sources ?? []).some((s) => isSourceBusy(s.status));
}
