import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Loader2, UsersRound } from 'lucide-react';
import { Empty } from '@/components/mds';
import { api, type TaskRoleTurn } from '@/lib/api';

export function TaskRolesPanel({ taskId }: { taskId: string }) {
  const intl = useIntl();
  const [turns, setTurns] = useState<TaskRoleTurn[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(false);
    api.tasks.roleTurns(taskId).then(({ turns: rows }) => {
      if (active) setTurns(rows);
    }).catch(() => {
      if (active) setError(true);
    }).finally(() => {
      if (active) setLoading(false);
    });
    return () => { active = false; };
  }, [taskId]);

  if (loading) return <div className="flex justify-center py-8"><Loader2 className="size-5 animate-spin" /></div>;
  if (error) return <p className="py-6 text-sm text-destructive">{intl.formatMessage({ id: 'tasks.roles.error' })}</p>;
  if (turns.length === 0) return <Empty icon={UsersRound} title={intl.formatMessage({ id: 'tasks.roles.empty' })} />;

  return (
    <ol className="space-y-2 py-3">
      {turns.map((turn, index) => (
        <li key={`${turn.round}-${turn.role}-${index}`} className="rounded-lg border border-surface-border p-3 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{intl.formatMessage({ id: `tasks.roles.role.${turn.role}` })}</span>
            <span className="text-muted-foreground">#{turn.round}</span>
            <span className="font-mono text-xs">{turn.response_model ?? turn.request_model ?? turn.runtime}</span>
            <span className="ml-auto text-xs text-muted-foreground">{intl.formatMessage({ id: `tasks.roles.outcome.${turn.outcome}` })}</span>
          </div>
          <div className="mt-1 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'tasks.roles.usage' })}: {turn.usage_input_tokens == null || turn.usage_output_tokens == null
              ? intl.formatMessage({ id: 'tasks.roles.unknown' })
              : `${turn.usage_input_tokens.toLocaleString()} / ${turn.usage_output_tokens.toLocaleString()}`}
            {turn.failover && ` · ${intl.formatMessage({ id: 'tasks.roles.failover' })}`}
          </div>
        </li>
      ))}
    </ol>
  );
}
