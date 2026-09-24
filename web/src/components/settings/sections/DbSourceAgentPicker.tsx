import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { Checkbox } from '@/components/mds';
import type { DbSourceGrantAgent } from '@/lib/api';
import { buildAgentPickerRows, toggleAgentSelection } from './dbSourceGrants';

/**
 * Shared "哪些 AI 員工可以使用這個資料庫來源" checkbox list — one component
 * behind the redaction 精靈 step 4 (db sources) and RedactionSystemsCard's
 * grant dialog, so both surfaces render/behave identically (WP-C). Archived
 * agents sort last and render muted, per the same convention as the AI
 * 員工 roster page, but stay selectable — an operator may be re-onboarding
 * one and shouldn't have to unarchive it first just to grant it a source.
 */
export function DbSourceAgentPicker({
  agents,
  selected,
  onChange,
  disabled,
}: {
  agents: readonly DbSourceGrantAgent[];
  selected: readonly string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const rows = buildAgentPickerRows(agents, selected);

  if (rows.length === 0) {
    return (
      <p className="rounded-lg bg-muted/50 px-3 py-2.5 text-xs text-muted-foreground">
        {intl.formatMessage({ id: 'redaction.dbGrants.picker.empty' })}
      </p>
    );
  }

  return (
    <div className="max-h-56 space-y-0.5 overflow-y-auto rounded-lg border border-surface-border p-1.5">
      {rows.map((row) => (
        <label
          key={row.id}
          className={cn(
            'flex cursor-pointer items-center gap-2.5 rounded-md px-2 py-1.5 transition-colors hover:bg-surface-hover',
            row.archived && 'opacity-60',
            disabled && 'pointer-events-none opacity-50',
          )}
        >
          <Checkbox
            checked={row.checked}
            disabled={disabled}
            onCheckedChange={() => onChange(toggleAgentSelection(selected, row.id))}
          />
          <span className="min-w-0 flex-1 truncate text-sm text-foreground">{row.displayName}</span>
          <code className="shrink-0 font-mono text-[11px] text-muted-foreground">{row.id}</code>
          {row.archived && (
            <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
              {intl.formatMessage({ id: 'status.archived' })}
            </span>
          )}
        </label>
      ))}
    </div>
  );
}
