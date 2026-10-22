import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { api, type ApprovalItem } from '@/lib/api';
import type { AgentFilePendingResponse, AgentFileVerdict } from '@/lib/agent-files-types';
import { DiffView, diffRows } from '@/components/agent-files/DiffView';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { cn } from '@/lib/utils';
import { agentFileLabel, agentFileName, isAgentFileKey, reasonCategoryText } from '@/components/agent-files/review-text';

/** Kept for existing callers; the diff view lives with the other profile file components. */
export { diffRows };

function verdictTone(v: AgentFileVerdict): string {
  return v === 'block' ? 'text-destructive' : v === 'warn' ? 'text-warning' : 'text-success';
}

function payloadField(payload: unknown, key: string): string {
  let p: unknown = payload;
  if (typeof p === 'string') {
    try {
      p = JSON.parse(p);
    } catch {
      return '';
    }
  }
  const v = p && typeof p === 'object' ? (p as Record<string, unknown>)[key] : undefined;
  return typeof v === 'string' ? v : '';
}

/**
 * What an admin decides on an `agent_file_change` card: which AI employee,
 * which file, who asked and why, what the automatic check said, and the
 * change itself. Content and diff come from `agent_files.pending_get`
 * (admins only); every server string is shown as plain text.
 */
export function AgentFileChangeFacts({ approval, agentName }: { approval: ApprovalItem; agentName?: string }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const role = useAuthStore((s) => s.user?.role);
  const isAdmin = hasMinRole(role, 'admin');
  const [data, setData] = useState<AgentFilePendingResponse | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!isAdmin) return;
    let alive = true;
    setData(null);
    setFailed(false);
    api.agentFiles
      .pendingGet(approval.id)
      .then((r) => alive && setData(r))
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [approval.id, isAdmin]);

  const file = data?.file ?? payloadField(approval.payload, 'file');
  const employee = agentName ?? data?.agent_id ?? approval.agent_id ?? payloadField(approval.payload, 'agent_id');
  const requestedBy = data?.requested_by ?? payloadField(approval.payload, 'requested_by');
  const reason = data?.reason ?? payloadField(approval.payload, 'reason');

  return (
    <section
      aria-label={t('agentFiles.approval.title')}
      className="space-y-3 rounded-lg border border-border p-3 text-sm"
      data-testid="agent-file-change"
    >
      <p className="font-medium">{t('agentFiles.approval.title')}</p>

      {data?.stale && (
        <p role="alert" className="flex items-start gap-2 rounded-md border border-warning/30 bg-warning/10 px-2 py-1.5 text-xs text-foreground">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-warning" aria-hidden="true" />
          {t('agentFiles.approval.stale')}
        </p>
      )}

      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <dt className="text-xs text-muted-foreground">{t('agentFiles.approval.employee')}</dt>
        <dd className="min-w-0 break-words">{employee || '—'}</dd>
        <dt className="text-xs text-muted-foreground">{t('agentFiles.approval.file')}</dt>
        <dd className="min-w-0">
          {file ? agentFileLabel(intl, file) : '—'}
          {isAgentFileKey(file) && (
            <span className="ml-1.5 font-mono text-[11px] text-muted-foreground">{agentFileName(file)}</span>
          )}
        </dd>
        <dt className="text-xs text-muted-foreground">{t('agentFiles.approval.requestedBy')}</dt>
        <dd className="min-w-0 break-words">{requestedBy || '—'}</dd>
        <dt className="text-xs text-muted-foreground">{t('agentFiles.approval.reason')}</dt>
        <dd className="min-w-0 whitespace-pre-wrap break-words">{reason || '—'}</dd>
      </dl>

      {!isAdmin ? (
        <p className="text-xs text-muted-foreground">{t('agentFiles.approval.adminOnly')}</p>
      ) : failed ? (
        <p role="alert" className="text-xs text-destructive">{t('agentFiles.approval.loadFailed')}</p>
      ) : !data ? (
        <p className="text-xs text-muted-foreground" role="status">{t('agentFiles.approval.loading')}</p>
      ) : (
        <>
          <div className="space-y-1">
            <p className="text-xs text-muted-foreground">{t('agentFiles.approval.check')}</p>
            <p className={cn('text-sm font-medium', verdictTone(data.verdict))}>{t(`agentFiles.verdict.${data.verdict}`)}</p>
            {data.reason_categories.length === 0 ? (
              <p className="text-xs text-muted-foreground">{t('agentFiles.approval.noReasons')}</p>
            ) : (
              <ul className="list-disc space-y-0.5 pl-4 text-xs text-foreground">
                {data.reason_categories.map((c, i) => (
                  <li key={`${c}-${i}`}>{reasonCategoryText(intl, c)}</li>
                ))}
              </ul>
            )}
          </div>

          <div className="space-y-1">
            <p className="text-xs text-muted-foreground">{t('agentFiles.approval.diff')}</p>
            {!data.content_available || !data.diff ? (
              <p className="text-xs text-muted-foreground">{t('agentFiles.approval.noContent')}</p>
            ) : (
              <>
                <p className="text-xs text-muted-foreground">
                  {t('agentFiles.approval.diffSummary', { added: data.diff.added, removed: data.diff.removed })}
                </p>
                <DiffView hunks={data.diff.hunks} />
              </>
            )}
          </div>
        </>
      )}
    </section>
  );
}
