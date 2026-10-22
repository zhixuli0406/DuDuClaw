import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import type { SkillStateStatus } from '@/lib/api';

/**
 * Employees whose skill switches could not be read. Those employees load no
 * skills at all until the switches are repaired, so this is shown loudly.
 */
export function SkillStateErrors({
  states,
  nameOf,
}: {
  states: Readonly<Record<string, SkillStateStatus>>;
  nameOf: (agentId: string) => string;
}) {
  const intl = useIntl();
  const broken = Object.entries(states).filter(([, s]) => !s.ok);
  if (broken.length === 0) return null;
  return (
    <div role="alert" className="space-y-1 rounded-xl border border-destructive/30 bg-destructive/10 p-3">
      {broken.map(([agent, s]) => (
        <div key={agent} className="flex items-start gap-2 text-sm text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <div className="min-w-0">
            <p>{intl.formatMessage({ id: 'skills.manage.stateError' }, { agent: nameOf(agent) })}</p>
            {s.error && <p className="font-mono text-xs break-all text-destructive/80">{s.error}</p>}
          </div>
        </div>
      ))}
    </div>
  );
}
