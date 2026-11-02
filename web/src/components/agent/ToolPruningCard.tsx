import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type ToolPruningMode, type ToolPruningStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { Button, Card, CardContent, CardHeader, CardTitle, useErrorMessage } from '@/components/mds';

/** Admin-only observability. Pruning never changes the employee's authority. */
export function ToolPruningCard({ agentId }: { agentId: string }) {
  const user = useAuthStore(s => s.user);
  const connected = useConnectionStore(s => s.state === 'authenticated');
  if (user?.role !== 'admin' || user.status !== 'active' || !connected) return null;
  return <ToolPruningCardContent key={`${agentId}:${user.id}`} agentId={agentId} />;
}

function ToolPruningCardContent({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const [status, setStatus] = useState<ToolPruningStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);
  const load = useCallback(async () => {
    const next = await api.agents.toolPruningStatus(agentId);
    if (alive.current) {
      setStatus(next);
      setError(null);
    }
  }, [agentId]);
  useEffect(() => {
    alive.current = true;
    void load().catch(e => { if (alive.current) setError(e); });
    return () => { alive.current = false; };
  }, [load]);

  async function change(action: () => Promise<unknown>) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await action();
      await load();
    } catch (e) {
      if (alive.current) setError(e);
    } finally {
      if (alive.current) setBusy(false);
    }
  }

  return (
    <Card data-size="sm">
      <CardHeader><CardTitle className="text-sm">{intl.formatMessage({ id: 'toolPruning.title' })}</CardTitle></CardHeader>
      <CardContent className="space-y-3 text-sm">
        {status && <>
          <label className="flex items-center justify-between gap-3">
            <span>{intl.formatMessage({ id: 'toolPruning.mode' })}</span>
            <select
              value={status.mode}
              disabled={busy}
              className="rounded-md border border-surface-border bg-background px-2 py-1 text-foreground"
              onChange={e => {
                const mode = e.target.value as ToolPruningMode;
                void change(() => api.agents.update(agentId, { capabilities: { tool_pruning: mode } }));
              }}
            >
              {(['off', 'shadow', 'on'] as const).map(mode => <option key={mode} value={mode}>{intl.formatMessage({ id: `toolPruning.mode.${mode}` })}</option>)}
            </select>
          </label>
          <p>{intl.formatMessage({ id: 'toolPruning.savings' }, { count: status.prunable_names.length, tokens: status.estimated_tokens })}</p>
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'toolPruning.listed' }, { count: status.listed_count })}</p>
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'toolPruning.discoveryOnly' })}</p>
          {status.prunable_names.length > 0 && <details>
            <summary className="cursor-pointer">{intl.formatMessage({ id: 'toolPruning.expand' })}</summary>
            <ul className="mt-2 max-h-48 space-y-1 overflow-y-auto">
              {status.prunable_names.map(name => <li key={name} className="break-all font-mono text-xs">{name}</li>)}
            </ul>
          </details>}
          {status.mode === 'on' && <Button size="sm" variant="outline" disabled={busy} onClick={() => void change(() => api.agents.toolPruningRestore(agentId))}>
            {intl.formatMessage({ id: 'toolPruning.restore' })}
          </Button>}
        </>}
        {!status && !error && <p role="status">{intl.formatMessage({ id: 'common.loading' })}</p>}
        {error != null && <div role="alert" className="space-y-2 text-destructive">
          <p>{errorText(error)}</p>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void change(async () => {})}>{intl.formatMessage({ id: 'toolPruning.retry' })}</Button>
        </div>}
      </CardContent>
    </Card>
  );
}
