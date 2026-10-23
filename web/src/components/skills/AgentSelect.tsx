import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import { glyphText } from '@/lib/agent-glyph';
import { sortAgentsByName } from '@/lib/skill-library';
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from '@/components/mds';

/** Sentinel agent-picker value for the aggregate ("every staffer") view. */
export const ALL_AGENTS = '__all__';

/**
 * Agent picker shared by the install / adopt / my-skills surfaces. Agents are
 * listed by display name with the UI locale's collator (B8), not in the
 * gateway's map order. `allOption`, when given, prepends an aggregate entry
 * whose value is {@link ALL_AGENTS}.
 */
export function AgentSelect({
  value,
  onValueChange,
  agents,
  placeholder,
  allOption,
}: {
  value: string;
  onValueChange: (v: string) => void;
  agents: ReadonlyArray<{ name: string; display_name: string; icon?: string }>;
  placeholder?: string;
  allOption?: string;
}) {
  const intl = useIntl();
  const sorted = useMemo(() => sortAgentsByName(agents, intl.locale), [agents, intl.locale]);
  const current = agents.find((a) => a.name === value);
  const label =
    value === ALL_AGENTS && allOption
      ? allOption
      : current
        ? `${glyphText(current.icon)} ${current.display_name || current.name}`
        : placeholder;
  return (
    <Select value={value} onValueChange={(v) => onValueChange(String(v))}>
      <SelectTrigger className="w-52">
        <SelectValue placeholder={placeholder}>{label}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {allOption && <SelectItem value={ALL_AGENTS}>{allOption}</SelectItem>}
        {sorted.map((a) => (
          <SelectItem key={a.name} value={a.name}>
            {glyphText(a.icon) + ' ' + (a.display_name || a.name)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
