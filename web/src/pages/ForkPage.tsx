import { useState, useCallback, useEffect } from 'react';
import { cn } from '@/lib/utils';
import { api, type ForkSummary, type ForkDetail, type ForkBranch } from '@/lib/api';
import { GitFork, RefreshCw, Trophy, CircleDot, AlertCircle, CheckCircle2 } from 'lucide-react';

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
  return (
    <div
      className={cn(
        'flex flex-col rounded-xl border p-4',
        isWinner
          ? 'border-amber-400 bg-amber-50 dark:border-amber-500/50 dark:bg-amber-500/10'
          : 'border-stone-200 bg-white dark:border-stone-700 dark:bg-stone-800'
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-1.5 font-mono text-xs text-stone-500">
          {isWinner && <Trophy className="h-3.5 w-3.5 text-amber-500" />}
          {branch.branch_id.slice(0, 8)}
        </span>
        <span className={cn('text-xs font-medium', STATE_STYLES[branch.state] ?? 'text-stone-500')}>
          {branch.state}
        </span>
      </div>
      {branch.steering && (
        <p className="mt-2 text-sm font-medium text-stone-700 dark:text-stone-200">
          {branch.steering}
        </p>
      )}
      <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-stone-50 p-2 text-xs text-stone-600 dark:bg-stone-900 dark:text-stone-300">
        {branch.output || '(no output yet)'}
      </pre>
      <div className="mt-3 flex items-center justify-between text-xs text-stone-500">
        <span>${branch.spent_usd.toFixed(4)} / ${branch.budget_usd.toFixed(2)}</span>
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
        <button
          onClick={() => onResolve(branch.branch_id)}
          className="mt-3 rounded-lg bg-amber-500 px-3 py-1.5 text-sm font-medium text-white transition-colors hover:bg-amber-600"
        >
          Select as winner
        </button>
      )}
    </div>
  );
}

export function ForkPage() {
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
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="flex items-center gap-2 text-2xl font-semibold text-stone-900 dark:text-stone-50">
            <GitFork className="h-6 w-6 text-amber-500" />
            Live Run Forking
          </h2>
          <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
            Competing branches explored in parallel; pick the winner the judge proposes — or
            override it.
          </p>
        </div>
        <button
          onClick={() => void loadForks()}
          className="flex items-center gap-1.5 rounded-lg border border-stone-200 px-3 py-2 text-sm font-medium text-stone-600 transition-colors hover:bg-stone-50 dark:border-stone-700 dark:text-stone-300 dark:hover:bg-stone-800"
        >
          <RefreshCw className={cn('h-4 w-4', loading && 'animate-spin')} />
          Refresh
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-rose-200 bg-rose-50 px-4 py-3 text-sm text-rose-700 dark:border-rose-500/30 dark:bg-rose-500/10 dark:text-rose-300">
          {error}
        </div>
      )}

      <div className="grid gap-6 lg:grid-cols-[20rem_1fr]">
        {/* Fork list */}
        <div className="space-y-2">
          {forks.length === 0 && !loading && (
            <p className="text-sm text-stone-400">No forks yet. Agents create them via fork_run.</p>
          )}
          {forks.map((f) => (
            <button
              key={f.fork_id}
              onClick={() => void inspect(f.fork_id)}
              className={cn(
                'flex w-full flex-col gap-1 rounded-xl border p-3 text-left transition-colors',
                selected?.fork_id === f.fork_id
                  ? 'border-amber-400 bg-amber-50 dark:border-amber-500/50 dark:bg-amber-500/10'
                  : 'border-stone-200 bg-white hover:bg-stone-50 dark:border-stone-700 dark:bg-stone-800 dark:hover:bg-stone-700/50'
              )}
            >
              <span className="flex items-center justify-between">
                <span className="font-mono text-xs text-stone-500">{f.fork_id.slice(0, 14)}</span>
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
                {f.merge_mode} · ${f.aggregate_spent_usd.toFixed(4)}
                {f.promoted && ' · promoted'}
              </span>
            </button>
          ))}
        </div>

        {/* Detail */}
        <div>
          {selected ? (
            <div className="space-y-4">
              <div className="rounded-xl border border-stone-200 bg-white p-4 dark:border-stone-700 dark:bg-stone-800">
                <p className="text-sm text-stone-700 dark:text-stone-200">{selected.prompt}</p>
                <p className="mt-2 text-xs text-stone-400">
                  {selected.merge_mode} · {selected.resolved ? 'resolved' : 'open'}
                  {selected.winner && ` · winner ${selected.winner.slice(0, 8)}`}
                </p>
              </div>
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
            <div className="flex h-48 items-center justify-center rounded-xl border border-dashed border-stone-200 text-sm text-stone-400 dark:border-stone-700">
              Select a fork to inspect its branches
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
