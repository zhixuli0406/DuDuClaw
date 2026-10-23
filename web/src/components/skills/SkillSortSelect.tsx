import { useIntl } from 'react-intl';
import { ArrowDownUp } from 'lucide-react';
import { SKILL_SORT_KEYS, type SkillSortKey } from '@/lib/skill-library';
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from '@/components/mds';

/** Sort picker for the skill library; `keys` narrows the offered orders. */
export function SkillSortSelect({
  value,
  onChange,
  keys = SKILL_SORT_KEYS,
}: {
  value: SkillSortKey;
  onChange: (key: SkillSortKey) => void;
  keys?: readonly SkillSortKey[];
}) {
  const intl = useIntl();
  const label = (k: SkillSortKey) => intl.formatMessage({ id: `skills.sort.${k}` });
  return (
    <Select value={value} onValueChange={(v) => onChange(v as SkillSortKey)}>
      <SelectTrigger className="w-40" aria-label={intl.formatMessage({ id: 'skills.sort.label' })}>
        <ArrowDownUp className="size-3.5 text-muted-foreground" />
        <SelectValue>{label(value)}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {keys.map((k) => (
          <SelectItem key={k} value={k}>
            {label(k)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
