import { useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/mds';

/** Sentinel for "獨立團隊，不掛接" (Select cannot carry an empty value). */
const NONE = '__none__';

/**
 * WP-ORG — supervisor picker for expert-pack installs. The pack's root agent
 * (its front desk) can report to an existing supervisor so a new team joins
 * the org chart instead of floating beside it. Candidates = active agents
 * that are `main` or already have direct reports (manager-rank in practice);
 * empty selection installs the pack as an independent root (the old default).
 */
export function AttachUnderSelect({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (agentName: string) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const { agents, fetchAgents, loaded } = useAgentsStore();

  useEffect(() => {
    if (!loaded) fetchAgents();
  }, [loaded, fetchAgents]);

  const supervisors = useMemo(() => {
    const hasReports = new Set(
      agents.map((a) => a.reports_to).filter((r): r is string => !!r),
    );
    return agents.filter(
      (a) =>
        !a.archived &&
        a.status !== 'terminated' &&
        (a.role === 'main' || hasReports.has(a.name)),
    );
  }, [agents]);

  const current = supervisors.find((a) => a.name === value);

  return (
    <div className="space-y-1.5">
      <label className="text-xs font-medium text-muted-foreground">
        {intl.formatMessage({ id: 'experts.attach.label' })}
      </label>
      <Select
        value={value || NONE}
        onValueChange={(v) => onChange(String(v) === NONE ? '' : String(v))}
        disabled={disabled}
      >
        <SelectTrigger size="sm" aria-label={intl.formatMessage({ id: 'experts.attach.label' })}>
          <SelectValue>
            {current
              ? current.display_name || current.name
              : intl.formatMessage({ id: 'experts.attach.none' })}
          </SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={NONE}>
            {intl.formatMessage({ id: 'experts.attach.none' })}
          </SelectItem>
          {supervisors.map((a) => (
            <SelectItem key={a.name} value={a.name}>
              {a.display_name || a.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <p className="text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'experts.attach.help' })}
      </p>
    </div>
  );
}
