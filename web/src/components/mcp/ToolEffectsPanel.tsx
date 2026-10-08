import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type McpServerToolEffects } from '@/lib/api';
import { Badge, Button, Empty } from '@/components/mds';
import { Loader2, RefreshCw, ShieldCheck } from 'lucide-react';
import { AgentSelect, type AgentLite } from './AgentSelect';

const VERDICT_CLASS: Record<string, string> = {
  allow: 'bg-success/15 text-success',
  ask: 'bg-warning/15 text-warning',
  block: 'bg-destructive/15 text-destructive',
};

/**
 * Third-party MCP tools of one employee with the effect class DuDuClaw
 * derives for them (2026-10-08). The list is what the employee's last
 * session saw through DuDuClaw's proxy or remote bridge; class and verdict
 * are recomputed from the current `[capabilities]` settings.
 */
export function ToolEffectsPanel({ agents }: { agents: ReadonlyArray<AgentLite> }) {
  const intl = useIntl();
  const [agentId, setAgentId] = useState(agents[0]?.name ?? '');
  const [servers, setServers] = useState<McpServerToolEffects[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!agentId && agents[0]) setAgentId(agents[0].name);
  }, [agents, agentId]);

  const load = useCallback(async () => {
    if (!agentId) return;
    setLoading(true);
    setError(null);
    try {
      const res = await api.mcp.toolEffects(agentId);
      setServers(res.servers ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [agentId]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <section className="space-y-3" data-testid="tool-effects">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h3 className="flex items-center gap-1.5 text-sm font-medium text-foreground">
            <ShieldCheck className="size-4 text-brand" />
            {intl.formatMessage({ id: 'mcp.toolEffects.title' })}
          </h3>
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.toolEffects.desc' })}</p>
        </div>
        <div className="flex items-center gap-2">
          <AgentSelect value={agentId} onChange={setAgentId} agents={agents} className="w-44" />
          <Button variant="outline" size="sm" onClick={() => void load()} aria-label={intl.formatMessage({ id: 'common.refresh' })}>
            <RefreshCw />
          </Button>
        </div>
      </div>
      {loading && servers === null ? (
        <div className="flex justify-center py-6">
          <Loader2 className="size-5 animate-spin text-brand" />
        </div>
      ) : error ? (
        <p className="text-sm text-destructive" role="alert">{error}</p>
      ) : !servers || servers.length === 0 ? (
        <Empty icon={ShieldCheck} title={intl.formatMessage({ id: 'mcp.toolEffects.empty' })} />
      ) : (
        servers.map((srv) => (
          <div key={srv.server} className="overflow-hidden rounded-xl border border-surface-border">
            <div className="flex flex-wrap items-center gap-2 border-b border-surface-border px-4 py-2">
              <span className="text-sm font-medium text-foreground">{srv.server}</span>
              <Badge variant="secondary">
                {intl.formatMessage({ id: srv.read_hint_trusted ? 'mcp.toolEffects.trusted' : 'mcp.toolEffects.untrusted' })}
              </Badge>
              <span className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'mcp.toolEffects.observed' }, { at: new Date(srv.observed_at).toLocaleString() })}
              </span>
            </div>
            <ul className="divide-y divide-surface-border">
              {srv.tools.map((t) => (
                <li key={t.name} className="flex flex-wrap items-center gap-2 px-4 py-2" data-testid="tool-effect-row">
                  <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground" title={t.description}>
                    {t.name}
                  </span>
                  <Badge variant="secondary">{intl.formatMessage({ id: `agents.actionRules.effect.${t.effect}` })}</Badge>
                  <Badge variant="secondary" className={VERDICT_CLASS[t.verdict]}>
                    {intl.formatMessage({ id: `agents.actionRules.verdict.${t.verdict}` })}
                  </Badge>
                  {!t.explore_visible && (
                    <Badge variant="secondary">{intl.formatMessage({ id: 'mcp.toolEffects.exploreHidden' })}</Badge>
                  )}
                </li>
              ))}
            </ul>
          </div>
        ))
      )}
      <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'mcp.toolEffects.trustHelp' })}</p>
    </section>
  );
}
