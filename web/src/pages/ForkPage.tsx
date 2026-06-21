import { useState, useCallback, useEffect } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type ForkSummary, type ForkDetail, type ForkBranch } from '@/lib/api';
import { GitFork, RefreshCw, Trophy, CircleDot, AlertCircle, CheckCircle2 } from 'lucide-react';
import { Page, PageHeader, Card, Button, Badge, EmptyState } from '@/components/ui';

// RFC-26 Live Run Forking dashboard. Reads fork state from the cross-process
// ForkStore via the gateway `fork.*` RPC. Forks execute in the MCP-server
// process; this view observes + resolves them.

const STATE_STYLES: Record<string, string> = {
  finished: 'text-emerald-600 dark:text-emerald-400',
  running: 'text-amber-600 dark:text-amber-400',
  pending: 'text-stone-400',
  budget_killed: 'text-rose-600 dark:text-rose-400',
  failed: 'text-rose-600 dark:text-rose-400',
  terminated: 'text-stone-500',
};

function BranchCard({
  branch,
  isWinner,
  canResolve,
  onResolve,
}: {
  branch: ForkBranch;
  isWinner: boolean;
  canResolve: boolean;
  onResolve: (branchId: string) => void;
}) {
  const intl = useIntl();
  return (
    <div
      className={cn(
        'flex flex-col rounded-xl border p-4',
        isWinner
          ? 'border-amber-400 bg-amber-50 dark:border-amber-500/50 dark:bg-amber-500/10'
          : 'border-[var(--panel-border)] bg-[var(--panel-fill)]'
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-1.5 font-mono text-xs text-stone-500">
          {isWinner && <Trophy className="h-3.5 w-3.5 text-amber-500" />}
          {branch.branch_id.slice(0, 8)}
        </span>
        <span
          className={cn('text-xs font-medium', STATE_STYLES[branch.state] ?? 'text-stone-500')}
        >
          {branch.state}
        </span>
      </div>
      {branch.steering && (
        <p className="mt-2 text-sm font-medium text-stone-700 dark:text-stone-200">
          {branch.steering}
        </p>
      )}
      <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-stone-500/5 p-2 text-xs text-stone-600 dark:bg-white/5 dark:text-stone-300">
        {branch.output || '(no output yet)'}
      </pre>
      <div className="mt-3 flex items-center justify-between text-xs text-stone-500">
        <span className="tabular-nums">
          ${branch.spent_usd.toFixed(4)} / ${branch.budget_usd.toFixed(2)}
        </span>
        {branch.test_exit_code !== null && (
          <span className="flex items-center gap-1">
            {branch.test_exit_code === 0 ? (
              <CheckCircle2 className="h-3.5 w-3.5 text-emerald-500" />
            ) : (
              <AlertCircle className="h-3.5 w-3.5 text-rose-500" />
            )}
            test {branch.test_exit_code}
          </span>
        )}
      </div>
      {canResolve && (
        <Button
          variant="primary"
          size="sm"
          className="mt-3"
          onClick={() => onResolve(branch.branch_id)}
        >
          {intl.formatMessage({ id: 'forks.selectWinner' })}
        </Button>
      )}
    </div>
  );
}

export function ForkPage() {
  const intl = useIntl();
  const [forks, setForks] = useState<ForkSummary[]>([]);
  const [selected, setSelected] = useState<ForkDetail | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadForks = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.fork.list(50);
      setForks(res.forks);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load forks');
    } finally {
      setLoading(false);
    }
  }, []);

  const inspect = useCallback(async (forkId: string) => {
    try {
      const detail = await api.fork.inspect(forkId);
      setSelected(detail);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to inspect fork');
    }
  }, []);

  const resolve = useCallback(
    async (branchId: string) => {
      if (!selected) return;
      try {
        await api.fork.resolve(selected.fork_id, branchId);
        await inspect(selected.fork_id);
        await loadForks();
      } catch (e) {
        setError(e instanceof Error ? e.message : 'Failed to resolve fork');
      }
    },
    [selected, inspect, loadForks]
  );

  useEffect(() => {
    void loadForks();
  }, [loadForks]);

  return (
    <Page>
      <PageHeader
        icon={GitFork}
        title={intl.formatMessage({ id: 'nav.forks' })}
        subtitle={intl.formatMessage({ id: 'forks.subtitle' })}
        actions={
          <Button
            variant="secondary"
            onClick={() => void loadForks()}
            icon={() => <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />}
          >
            {intl.formatMessage({ id: 'common.refresh' })}
          </Button>
        }
      />

      {error && (
        <div className="rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-500/30 dark:bg-rose-500/10 dark:text-rose-300">
          {error}
        </div>
      )}

      <div className="grid gap-6 lg:grid-cols-[20rem_1fr]">
        {/* Fork list */}
        <div className="space-y-2">
          {forks.length === 0 && !loading && (
            <Card>
              <EmptyState
                icon={GitFork}
                title={intl.formatMessage({ id: 'forks.empty.title' })}
                hint={intl.formatMessage({ id: 'forks.empty.hint' })}
              />
            </Card>
          )}
          {forks.map((f) => (
            <Card
              key={f.fork_id}
              interactive
              onClick={() => void inspect(f.fork_id)}
              padded={false}
              className={cn(
                selected?.fork_id === f.fork_id &&
                  'border-amber-400 bg-amber-50 dark:border-amber-500/50 dark:bg-amber-500/10'
              )}
            >
              <div className="flex flex-col gap-1 p-3 text-left">
                <span className="flex items-center justify-between">
                  <span className="font-mono text-xs text-stone-500">
                    {f.fork_id.slice(0, 14)}
                  </span>
                  {f.resolved ? (
                    <Trophy className="h-3.5 w-3.5 text-amber-500" />
                  ) : (
                    <CircleDot className="h-3.5 w-3.5 text-amber-500" />
                  )}
                </span>
                <span className="text-sm font-medium text-stone-700 dark:text-stone-200">
                  {f.agent_id}
                </span>
                <span className="text-xs text-stone-400">
                  {f.merge_mode} · <span className="tabular-nums">${f.aggregate_spent_usd.toFixed(4)}</span>
                  {f.promoted && ' · promoted'}
                </span>
              </div>
            </Card>
          ))}
        </div>

        {/* Detail */}
        <div>
          {selected ? (
            <div className="space-y-4">
              <Card>
                <p className="text-sm text-stone-700 dark:text-stone-200">{selected.prompt}</p>
                <p className="mt-2 flex flex-wrap items-center gap-1.5 text-xs text-stone-400">
                  <Badge tone="neutral">{selected.merge_mode}</Badge>
                  <Badge tone={selected.resolved ? 'success' : 'warning'}>
                    {selected.resolved ? 'resolved' : 'open'}
                  </Badge>
                  {selected.winner && (
                    <Badge tone="accent">winner {selected.winner.slice(0, 8)}</Badge>
                  )}
                </p>
              </Card>
              <div className="grid gap-4 md:grid-cols-2">
                {selected.branches.map((b) => (
                  <BranchCard
                    key={b.branch_id}
                    branch={b}
                    isWinner={selected.winner === b.branch_id}
                    canResolve={!selected.resolved}
                    onResolve={(bid) => void resolve(bid)}
                  />
                ))}
              </div>
            </div>
          ) : (
            <Card>
              <EmptyState
                icon={GitFork}
                title={intl.formatMessage({ id: 'forks.detail.empty' })}
              />
            </Card>
          )}
        </div>
      </div>
    </Page>
  );
}
