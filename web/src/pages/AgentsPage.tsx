import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { cn } from '@/lib/utils';
import { Bot, Pause, Play, Send, Eye, Plus } from 'lucide-react';

function StatusBadge({ status }: { status: string }) {
  const intl = useIntl();
  const styles: Record<string, string> = {
    active:
      'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400',
    paused:
      'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400',
    terminated:
      'bg-rose-100 text-rose-700 dark:bg-rose-900/30 dark:text-rose-400',
  };

  return (
    <span
      className={cn(
        'inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium',
        styles[status] ?? 'bg-stone-100 text-stone-600'
      )}
    >
      {intl.formatMessage({ id: `status.${status}` })}
    </span>
  );
}

function RoleBadge({ role }: { role: string }) {
  const intl = useIntl();
  return (
    <span className="inline-flex items-center rounded-full bg-stone-100 px-2 py-0.5 text-xs text-stone-600 dark:bg-stone-800 dark:text-stone-400">
      {intl.formatMessage({ id: `agents.role.${role}` })}
    </span>
  );
}

export function AgentsPage() {
  const intl = useIntl();
  const { agents, fetchAgents, pauseAgent, resumeAgent, loading } =
    useAgentsStore();

  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold text-stone-900 dark:text-stone-50">
          {intl.formatMessage({ id: 'agents.title' })}
        </h2>
        <button className="inline-flex items-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600">
          <Plus className="h-4 w-4" />
          {intl.formatMessage({ id: 'agents.create' })}
        </button>
      </div>

      {agents.length === 0 && !loading ? (
        <div className="flex flex-col items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-16 dark:border-stone-700 dark:bg-stone-900">
          <Bot className="mb-4 h-12 w-12 text-stone-300 dark:text-stone-600" />
          <p className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'agents.empty' })}
          </p>
        </div>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {agents.map((agent) => (
            <div
              key={agent.name}
              className="rounded-xl border border-stone-200 bg-white p-5 transition-shadow hover:shadow-md dark:border-stone-800 dark:bg-stone-900"
            >
              <div className="flex items-start justify-between">
                <div className="flex items-center gap-3">
                  <span className="text-2xl">{agent.icon || '🤖'}</span>
                  <div>
                    <h3 className="font-semibold text-stone-900 dark:text-stone-50">
                      {agent.display_name}
                    </h3>
                    <p className="text-xs text-stone-500 dark:text-stone-400">
                      {agent.trigger}
                    </p>
                  </div>
                </div>
                <StatusBadge status={agent.status} />
              </div>

              <div className="mt-3 flex items-center gap-2">
                <RoleBadge role={agent.role} />
              </div>

              {/* Budget bar */}
              {agent.budget && (
                <div className="mt-4">
                  <div className="mb-1 flex justify-between text-xs text-stone-500 dark:text-stone-400">
                    <span>
                      {intl.formatMessage({ id: 'dashboard.budget.title' })}
                    </span>
                    <span>
                      ${(agent.budget.spent_cents / 100).toFixed(2)} / $
                      {(agent.budget.monthly_limit_cents / 100).toFixed(2)}
                    </span>
                  </div>
                  <div className="h-1.5 overflow-hidden rounded-full bg-stone-200 dark:bg-stone-700">
                    <div
                      className="h-full rounded-full bg-amber-500 transition-all"
                      style={{
                        width: `${Math.min(
                          100,
                          (agent.budget.spent_cents /
                            agent.budget.monthly_limit_cents) *
                            100
                        )}%`,
                      }}
                    />
                  </div>
                </div>
              )}

              {/* Actions */}
              <div className="mt-4 flex gap-2 border-t border-stone-100 pt-3 dark:border-stone-800">
                {agent.status === 'active' ? (
                  <button
                    onClick={() => pauseAgent(agent.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
                  >
                    <Pause className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'agents.pause' })}
                  </button>
                ) : (
                  <button
                    onClick={() => resumeAgent(agent.name)}
                    className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-emerald-600 hover:bg-emerald-50 dark:text-emerald-400 dark:hover:bg-emerald-900/20"
                  >
                    <Play className="h-3.5 w-3.5" />
                    {intl.formatMessage({ id: 'agents.resume' })}
                  </button>
                )}
                <button className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800">
                  <Send className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'agents.delegate' })}
                </button>
                <button className="inline-flex items-center gap-1 rounded-md px-2.5 py-1.5 text-xs text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800">
                  <Eye className="h-3.5 w-3.5" />
                  {intl.formatMessage({ id: 'agents.inspect' })}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
