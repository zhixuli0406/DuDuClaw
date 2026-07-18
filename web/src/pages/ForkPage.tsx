import { useState, useCallback, useEffect } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api, type ForkSummary, type ForkDetail, type ForkBranch } from '@/lib/api';
import { GitBranch, RefreshCw, Trophy, CircleDot, AlertCircle, CheckCircle2 } from 'lucide-react';
import {
  CollectionPageHeader,
  Card,
  CardContent,
  Button,
  Badge,
  Empty,
  ActorAvatar,
} from '@/components/mds';

// RFC-26 Live Run Forking dashboard. Reads fork state from the cross-process
// ForkStore via the gateway `fork.*` RPC. Forks execute in the MCP-server
// process; this view observes + resolves them.

const STATE_STYLES: Record<string, string> = {
  finished: 'text-success',
  running: 'text-brand',
  pending: 'text-muted-foreground',
  budget_killed: 'text-destructive',
  failed: 'text-destructive',
  terminated: 'text-muted-foreground',
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
    <Card className={cn('gap-3', isWinner && 'border-brand ring-1 ring-brand/40')}>
      <CardContent className="flex flex-1 flex-col gap-3">
        <div className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-1.5 font-mono text-xs text-muted-foreground">
            {isWinner && <Trophy className="size-3.5 text-brand" />}
            {branch.branch_id.slice(0, 8)}
          </span>
          <span className={cn('text-xs font-medium', STATE_STYLES[branch.state] ?? 'text-muted-foreground')}>
            {branch.state}
          </span>
        </div>
        {branch.steering && (
          <p className="text-sm font-medium text-foreground">{branch.steering}</p>
        )}
        <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-muted p-2 text-xs text-muted-foreground">
          {branch.output || intl.formatMessage({ id: 'forks.branch.noOutput' })}
        </pre>
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span className="font-mono tabular-nums">
            ${branch.spent_usd.toFixed(4)} / ${branch.budget_usd.toFixed(2)}
          </span>
          {branch.test_exit_code !== null && (
            <span className="flex items-center gap-1 font-mono tabular-nums">
              {branch.test_exit_code === 0 ? (
                <CheckCircle2 className="size-3.5 text-success" />
              ) : (
                <AlertCircle className="size-3.5 text-destructive" />
              )}
              test {branch.test_exit_code}
            </span>
          )}
        </div>
      </CardContent>
      {canResolve && (
        <div className="border-t border-surface-border px-4 pt-3">
          <Button variant="brand" size="sm" onClick={() => onResolve(branch.branch_id)}>
            <Trophy />
            {intl.formatMessage({ id: 'forks.selectWinner' })}
          </Button>
        </div>
      )}
    </Card>
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
    [selected, inspect, loadForks],
  );

  useEffect(() => {
    void loadForks();
  }, [loadForks]);

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={GitBranch}
        title={intl.formatMessage({ id: 'nav.forks' })}
        count={forks.length}
        description={intl.formatMessage({ id: 'forks.subtitle' })}
        action={
          <Button variant="outline" size="sm" onClick={() => void loadForks()}>
            <RefreshCw className={cn(loading && 'animate-spin')} />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'common.refresh' })}</span>
          </Button>
        }
      />

      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {error && (
          <div className="rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">{error}</div>
        )}

        <div className="grid gap-6 lg:grid-cols-[20rem_1fr]">
          {/* Fork list — slim rows */}
          <div className="space-y-1">
            {forks.length === 0 && !loading ? (
              <Card>
                <Empty
                  icon={GitBranch}
                  title={intl.formatMessage({ id: 'forks.empty.title' })}
                  description={intl.formatMessage({ id: 'forks.empty.hint' })}
                />
              </Card>
            ) : (
              forks.map((f) => {
                const active = selected?.fork_id === f.fork_id;
                return (
                  <button
                    key={f.fork_id}
                    type="button"
                    onClick={() => void inspect(f.fork_id)}
                    aria-current={active ? 'true' : undefined}
                    className={cn(
                      'flex w-full flex-col gap-1 rounded-lg border px-3 py-2.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/50',
                      active
                        ? 'border-brand bg-brand/5'
                        : 'border-surface-border bg-surface hover:bg-surface-hover',
                    )}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className="font-mono text-xs text-muted-foreground">{f.fork_id.slice(0, 14)}</span>
                      {f.resolved ? (
                        <Trophy className="size-3.5 shrink-0 text-brand" />
                      ) : (
                        <CircleDot className="size-3.5 shrink-0 text-brand" />
                      )}
                    </span>
                    <span className="flex items-center gap-1.5 text-sm font-medium text-foreground">
                      <ActorAvatar actorType="agent" size="md" name={f.agent_id} />
                      <span className="truncate">{f.agent_id}</span>
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {f.merge_mode} ·{' '}
                      <span className="font-mono tabular-nums">${f.aggregate_spent_usd.toFixed(4)}</span>
                      {f.promoted && ` · ${intl.formatMessage({ id: 'forks.promoted' })}`}
                    </span>
                  </button>
                );
              })
            )}
          </div>

          {/* Detail */}
          <div>
            {selected ? (
              <div className="space-y-4">
                <Card>
                  <CardContent className="space-y-2">
                    <p className="text-sm text-foreground">{selected.prompt}</p>
                    <p className="flex flex-wrap items-center gap-1.5">
                      <Badge variant="secondary">{selected.merge_mode}</Badge>
                      <Badge variant="secondary" className={cn(selected.resolved ? 'text-success' : 'text-warning')}>
                        {intl.formatMessage({ id: selected.resolved ? 'forks.state.resolved' : 'forks.state.open' })}
                      </Badge>
                      {selected.winner && (
                        <Badge variant="secondary" className="font-mono">
                          {intl.formatMessage({ id: 'forks.winner' }, { id: selected.winner.slice(0, 8) })}
                        </Badge>
                      )}
                    </p>
                  </CardContent>
                </Card>
                <div className="grid gap-4 sm:grid-cols-2">
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
                <Empty icon={GitBranch} title={intl.formatMessage({ id: 'forks.detail.empty' })} />
              </Card>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
