import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { OrgChart } from '@/components/OrgChart';
import type { AgentDetail } from '@/lib/api';
import { Network, Pause, Play, X } from 'lucide-react';
import { Page, PageHeader, Card, Badge, Button } from '@/components/ui';

export function OrgChartPage() {
  const intl = useIntl();
  const { agents, fetchAgents, pauseAgent, resumeAgent } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState<AgentDetail | null>(null);

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  const handleNodeClick = (agentName: string) => {
    const agent = agents.find((a) => a.name === agentName) ?? null;
    setSelectedAgent(agent);
  };

  return (
    <Page>
      <PageHeader
        icon={Network}
        title={intl.formatMessage({ id: 'nav.org' })}
        subtitle={intl.formatMessage(
          { id: 'orgchart.subtitle' },
          { count: agents.length },
        )}
      />

      <Card padded={false} bodyClassName="p-2">
        <OrgChart
          agents={agents}
          onNodeClick={handleNodeClick}
          labels={{
            main: intl.formatMessage({ id: 'orgchart.legend.main' }),
            specialist: intl.formatMessage({ id: 'orgchart.legend.specialist' }),
            worker: intl.formatMessage({ id: 'orgchart.legend.worker' }),
            zoom: intl.formatMessage({ id: 'orgchart.zoom' }),
          }}
        />
      </Card>

      {/* Agent detail panel */}
      {selectedAgent && (
        <AgentDetailPanel
          agent={selectedAgent}
          onClose={() => setSelectedAgent(null)}
          onPause={async () => {
            await pauseAgent(selectedAgent.name);
            // Functional update: the panel may already be closed (null) by
            // the time the async call resolves.
            setSelectedAgent((prev) => (prev ? { ...prev, status: 'paused' } : null));
          }}
          onResume={async () => {
            await resumeAgent(selectedAgent.name);
            setSelectedAgent((prev) => (prev ? { ...prev, status: 'active' } : null));
          }}
        />
      )}
    </Page>
  );
}

// ── Agent Detail Side Panel ─────────────────────────────────

const STATUS_TONE: Record<string, 'success' | 'warning' | 'danger'> = {
  active: 'success',
  paused: 'warning',
  terminated: 'danger',
};

function AgentDetailPanel({
  agent,
  onClose,
  onPause,
  onResume,
}: {
  agent: AgentDetail;
  onClose: () => void;
  onPause: () => Promise<void>;
  onResume: () => Promise<void>;
}) {
  const intl = useIntl();

  return (
    <div className="glass-overlay fixed inset-y-0 right-0 z-50 flex w-96 flex-col border-l border-[var(--panel-border)]">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-[var(--panel-border)] px-6 py-4">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{agent.icon || '\u{1F916}'}</span>
          <div>
            <h3 className="font-semibold text-stone-900 dark:text-stone-50">
              {agent.display_name}
            </h3>
            <Badge tone={STATUS_TONE[agent.status] ?? 'neutral'} className="mt-0.5">
              {intl.formatMessage({ id: `status.${agent.status}` })}
            </Badge>
          </div>
        </div>
        <Button
          variant="ghost"
          size="sm"
          icon={X}
          onClick={onClose}
          aria-label={intl.formatMessage({ id: 'toast.close' })}
        />
      </div>

      {/* Body */}
      <div className="flex-1 space-y-5 overflow-y-auto px-6 py-5">
        <InfoRow label={intl.formatMessage({ id: 'orgchart.detail.name' })} value={agent.name} />
        <InfoRow
          label={intl.formatMessage({ id: 'orgchart.detail.role' })}
          value={intl.formatMessage({ id: `agents.role.${agent.role}` })}
        />
        <InfoRow label={intl.formatMessage({ id: 'orgchart.detail.trigger' })} value={agent.trigger} />
        <InfoRow
          label={intl.formatMessage({ id: 'orgchart.detail.reportsTo' })}
          value={agent.reports_to || intl.formatMessage({ id: 'orgchart.root' })}
        />
        <InfoRow
          label={intl.formatMessage({ id: 'orgchart.detail.model' })}
          value={agent.model?.preferred ?? 'N/A'}
        />

        {agent.budget && (
          <div>
            <p className="mb-1 text-xs font-medium uppercase tracking-wider text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'orgchart.detail.budget' })}
            </p>
            <div className="rounded-lg bg-stone-500/5 p-3 dark:bg-white/5">
              <div className="mb-2 flex justify-between text-sm">
                <span className="text-stone-600 dark:text-stone-300 tabular-nums">
                  ${(agent.budget.spent_cents / 100).toFixed(2)} / $
                  {(agent.budget.monthly_limit_cents / 100).toFixed(2)}
                </span>
                <span className="text-stone-400 tabular-nums">
                  {agent.budget.monthly_limit_cents > 0
                    ? Math.round(
                        (agent.budget.spent_cents /
                          agent.budget.monthly_limit_cents) *
                          100,
                      )
                    : 0}
                  %
                </span>
              </div>
              <div className="h-2 overflow-hidden rounded-full bg-stone-500/15 dark:bg-white/10">
                <div
                  className="h-full rounded-full bg-amber-500 transition-all"
                  style={{
                    width: `${Math.min(100, agent.budget.monthly_limit_cents > 0 ? (agent.budget.spent_cents / agent.budget.monthly_limit_cents) * 100 : 0)}%`,
                  }}
                />
              </div>
            </div>
          </div>
        )}

        {agent.skills && agent.skills.length > 0 && (
          <div>
            <p className="mb-1.5 text-xs font-medium uppercase tracking-wider text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'orgchart.detail.skills' })}
            </p>
            <div className="flex flex-wrap gap-1.5">
              {agent.skills.map((s) => (
                <Badge key={s} tone="neutral">
                  {s}
                </Badge>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Actions */}
      <div className="flex gap-2 border-t border-[var(--panel-border)] px-6 py-4">
        {agent.status === 'active' ? (
          <Button variant="secondary" icon={Pause} onClick={onPause} className="flex-1">
            {intl.formatMessage({ id: 'agents.pause' })}
          </Button>
        ) : (
          <Button variant="secondary" icon={Play} onClick={onResume} className="flex-1">
            {intl.formatMessage({ id: 'agents.resume' })}
          </Button>
        )}
      </div>
    </div>
  );
}

function InfoRow({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <p className="text-xs font-medium uppercase tracking-wider text-stone-400 dark:text-stone-500">
        {label}
      </p>
      <p className="mt-0.5 text-sm text-stone-900 dark:text-stone-100">
        {value}
      </p>
    </div>
  );
}
