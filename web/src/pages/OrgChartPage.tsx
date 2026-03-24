import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { OrgChart } from '@/components/OrgChart';
import { buttonSecondary } from '@/components/shared/Dialog';
import type { AgentDetail } from '@/lib/api';
import { Pause, Play } from 'lucide-react';

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
    <div className="flex h-full flex-col space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
            {intl.formatMessage({ id: 'orgchart.title' })}
          </h2>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage(
              { id: 'orgchart.subtitle' },
              { count: agents.length },
            )}
          </p>
        </div>
      </div>

      <div className="flex-1">
        <OrgChart agents={agents} onNodeClick={handleNodeClick} />
      </div>

      {/* Agent detail panel */}
      {selectedAgent && (
        <AgentDetailPanel
          agent={selectedAgent}
          onClose={() => setSelectedAgent(null)}
          onPause={async () => {
            await pauseAgent(selectedAgent.name);
            setSelectedAgent({
              ...selectedAgent,
              status: 'paused',
            });
          }}
          onResume={async () => {
            await resumeAgent(selectedAgent.name);
            setSelectedAgent({
              ...selectedAgent,
              status: 'active',
            });
          }}
        />
      )}
    </div>
  );
}

// ── Agent Detail Side Panel ─────────────────────────────────

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
  const statusStyles: Record<string, string> = {
    active:
      'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
    paused:
      'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
    terminated:
      'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
  };

  return (
    <div className="fixed inset-y-0 right-0 z-50 flex w-96 flex-col border-l border-stone-200 bg-white shadow-xl dark:border-stone-800 dark:bg-stone-900">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-stone-200 px-6 py-4 dark:border-stone-800">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{agent.icon || '\u{1F916}'}</span>
          <div>
            <h3 className="font-semibold text-stone-900 dark:text-stone-50">
              {agent.display_name}
            </h3>
            <span
              className={`mt-0.5 inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${statusStyles[agent.status] ?? ''}`}
            >
              {intl.formatMessage({ id: `status.${agent.status}` })}
            </span>
          </div>
        </div>
        <button
          onClick={onClose}
          className="rounded-lg p-1.5 text-stone-400 hover:bg-stone-100 hover:text-stone-600 dark:hover:bg-stone-800"
        >
          <svg className="h-5 w-5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
          </svg>
        </button>
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
          value={agent.reports_to || '(root)'}
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
            <div className="rounded-lg bg-stone-50 p-3 dark:bg-stone-800/50">
              <div className="mb-2 flex justify-between text-sm">
                <span className="text-stone-600 dark:text-stone-300">
                  ${(agent.budget.spent_cents / 100).toFixed(2)} / $
                  {(agent.budget.monthly_limit_cents / 100).toFixed(2)}
                </span>
                <span className="text-stone-400">
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
              <div className="h-2 overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
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
                <span
                  key={s}
                  className="rounded-full bg-stone-100 px-2.5 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400"
                >
                  {s}
                </span>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Actions */}
      <div className="flex gap-2 border-t border-stone-200 px-6 py-4 dark:border-stone-800">
        {agent.status === 'active' ? (
          <button onClick={onPause} className={buttonSecondary + ' flex-1 gap-1.5'}>
            <Pause className="h-4 w-4" />
            {intl.formatMessage({ id: 'agents.pause' })}
          </button>
        ) : (
          <button onClick={onResume} className={buttonSecondary + ' flex-1 gap-1.5'}>
            <Play className="h-4 w-4" />
            {intl.formatMessage({ id: 'agents.resume' })}
          </button>
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
