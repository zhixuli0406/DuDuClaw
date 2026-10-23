import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import type { AgentFileDiffResponse, AgentFileKey, AgentFileVersion } from '@/lib/agent-files-types';
import {
  Badge,
  Button,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  Spinner,
} from '@/components/mds';
import { DiffView } from './DiffView';

/** Where a version came from, in plain words (unknown sources read "other"). */
const SOURCE_KEYS: Readonly<Record<string, string>> = {
  dashboard: 'agentFiles.history.source.dashboard',
  rollback: 'agentFiles.history.source.rollback',
  rename: 'agentFiles.history.source.rename',
  'agents.update': 'agentFiles.history.source.editPage',
  'contract.update': 'agentFiles.history.source.editPage',
  'config.raw': 'agentFiles.history.source.advanced',
  approval: 'agentFiles.history.source.approval',
  initial: 'agentFiles.history.source.initial',
  mcp: 'agentFiles.history.source.tool',
  'chat.model': 'agentFiles.history.source.chat',
};

export function sourceLabelKey(source: string): string {
  return SOURCE_KEYS[source] ?? 'agentFiles.history.source.other';
}

export interface HistoryDrawerProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  agentId: string;
  file: AgentFileKey;
  /** Plain name of the file, for the title. */
  fileLabel: string;
  /** The hash of the file as it is now (that version cannot be "restored"). */
  currentHash: string | null;
  /** This person may restore (its owner or an admin). */
  canRestore: boolean;
  onRestore: (version: AgentFileVersion) => void;
}

type Detail =
  | { kind: 'view'; versionId: string; content: string | null; error: boolean }
  | { kind: 'compare'; versionId: string; diff: AgentFileDiffResponse | null; error: boolean };

/**
 * HistoryDrawer — the kept versions of one profile file, newest first: when,
 * who, from where; each can be viewed, compared with the current text, or
 * restored (restoring runs the same check as any other save).
 */
export function HistoryDrawer({
  open,
  onOpenChange,
  agentId,
  file,
  fileLabel,
  currentHash,
  canRestore,
  onRestore,
}: HistoryDrawerProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [versions, setVersions] = useState<AgentFileVersion[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [detail, setDetail] = useState<Detail | null>(null);

  useEffect(() => {
    if (!open) return;
    let alive = true;
    setVersions(null);
    setFailed(false);
    setDetail(null);
    api.agentFiles
      .history(agentId, file)
      .then((r) => alive && setVersions(r?.versions ?? []))
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [open, agentId, file]);

  const view = useCallback(
    (versionId: string) => {
      setDetail({ kind: 'view', versionId, content: null, error: false });
      api.agentFiles
        .get(agentId, file, versionId)
        .then((r) => setDetail((d) => (d?.versionId === versionId && d.kind === 'view' ? { ...d, content: r.content } : d)))
        .catch(() => setDetail((d) => (d?.versionId === versionId && d.kind === 'view' ? { ...d, error: true } : d)));
    },
    [agentId, file],
  );

  const compare = useCallback(
    (versionId: string) => {
      setDetail({ kind: 'compare', versionId, diff: null, error: false });
      api.agentFiles
        .diff(agentId, file, { version_id: versionId })
        .then((r) => setDetail((d) => (d?.versionId === versionId && d.kind === 'compare' ? { ...d, diff: r } : d)))
        .catch(() => setDetail((d) => (d?.versionId === versionId && d.kind === 'compare' ? { ...d, error: true } : d)));
    },
    [agentId, file],
  );

  // Display name from the server; an account that no longer exists reads as such (never the raw id).
  const who = (v: AgentFileVersion) =>
    v.user_id === 'system' || v.user_id === ''
      ? t('agentFiles.history.system')
      : v.user_name?.trim()
        ? v.user_name
        : t('agentFiles.history.deletedUser');

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full max-w-lg overflow-y-auto sm:max-w-lg" data-testid="history-drawer">
        <SheetHeader>
          <SheetTitle>{t('agentFiles.history.title', { file: fileLabel })}</SheetTitle>
          <SheetDescription>{t('agentFiles.history.description')}</SheetDescription>
        </SheetHeader>

        {failed ? (
          <p role="alert" className="text-sm text-destructive">
            {t('agentFiles.history.loadFailed')}
          </p>
        ) : versions === null ? (
          <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner label={t('agentFiles.history.loading')} />
            {t('agentFiles.history.loading')}
          </p>
        ) : versions.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('agentFiles.history.empty')}</p>
        ) : (
          <ol className="space-y-2" aria-label={t('agentFiles.history.title', { file: fileLabel })}>
            {versions.map((v) => {
              const isCurrent = currentHash !== null && v.sha256 === currentHash;
              const selected = detail?.versionId === v.version_id;
              return (
                <li key={v.version_id} className="space-y-2 rounded-lg border border-surface-border p-2.5" data-testid="history-version">
                  <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
                    <time dateTime={v.at} className="font-medium text-foreground">
                      {intl.formatDate(v.at, { dateStyle: 'medium', timeStyle: 'short' })}
                    </time>
                    <span className="text-muted-foreground">{t(sourceLabelKey(v.source))}</span>
                    {isCurrent && <Badge variant="secondary">{t('agentFiles.history.current')}</Badge>}
                  </div>
                  <p className="text-xs text-muted-foreground">{t('agentFiles.history.by', { name: who(v) })}</p>
                  {v.confirm_reason?.trim() && (
                    <p className="break-words text-xs text-muted-foreground" data-testid="history-reason">
                      {t('agentFiles.history.reason', { reason: v.confirm_reason.trim() })}
                    </p>
                  )}
                  <div className="flex flex-wrap gap-1.5">
                    <Button size="xs" variant="outline" onClick={() => view(v.version_id)} aria-pressed={selected && detail?.kind === 'view'}>
                      {t('agentFiles.history.view')}
                    </Button>
                    <Button
                      size="xs"
                      variant="outline"
                      onClick={() => compare(v.version_id)}
                      disabled={isCurrent}
                      aria-pressed={selected && detail?.kind === 'compare'}
                    >
                      {t('agentFiles.history.compare')}
                    </Button>
                    {canRestore && (
                      <Button size="xs" variant="outline" onClick={() => onRestore(v)} disabled={isCurrent}>
                        {t('agentFiles.history.restore')}
                      </Button>
                    )}
                  </div>
                  {selected && detail && <VersionDetail detail={detail} />}
                </li>
              );
            })}
          </ol>
        )}
      </SheetContent>
    </Sheet>
  );
}

function VersionDetail({ detail }: { detail: Detail }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  if (detail.error) {
    return (
      <p role="alert" className="text-xs text-destructive">
        {t('agentFiles.history.detailFailed')}
      </p>
    );
  }
  if (detail.kind === 'view') {
    if (detail.content === null) return <Spinner label={t('agentFiles.history.loading')} />;
    return (
      <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-muted p-2 font-mono text-xs" data-testid="history-content">
        {detail.content === '' ? t('agentFiles.editor.empty') : detail.content}
      </pre>
    );
  }
  if (detail.diff === null) return <Spinner label={t('agentFiles.history.loading')} />;
  return (
    <div className="space-y-1">
      <p className="text-xs text-muted-foreground">
        {t('agentFiles.history.compareSummary', { added: detail.diff.added, removed: detail.diff.removed })}
      </p>
      <DiffView hunks={detail.diff.hunks} />
    </div>
  );
}
