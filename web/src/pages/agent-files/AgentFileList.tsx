import { useIntl } from 'react-intl';
import { Badge } from '@/components/mds';
import { cn } from '@/lib/utils';
import type { AgentFileKey, AgentFileListEntry } from '@/lib/agent-files-types';
import { agentFileHeading, agentFileName, isSkillKey } from '@/components/agent-files/review-text';

export interface AgentFileListProps {
  files: ReadonlyArray<AgentFileListEntry>;
  selected: AgentFileKey;
  onSelect: (file: AgentFileKey) => void;
}

function Item({ entry, selected, onSelect }: { entry: AgentFileListEntry; selected: boolean; onSelect: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const readOnlyFile = entry.file === 'agent_toml';
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        aria-current={selected ? 'true' : undefined}
        className={cn(
          'flex w-full flex-col items-start gap-1 rounded-lg px-3 py-2 text-left transition-colors duration-150',
          'hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
          selected && 'bg-muted',
        )}
        data-testid={`agent-file-${entry.file}`}
      >
        <span className="text-sm font-medium text-foreground">{agentFileHeading(intl, entry.file)}</span>
        <span className="max-w-full truncate font-mono text-[11px] text-muted-foreground">{agentFileName(entry.file)}</span>
        <span className="flex flex-wrap gap-1">
          {entry.pending_approval_id && <Badge variant="secondary">{t('agentFiles.badge.pending')}</Badge>}
          {entry.drift === 'drifted' && <Badge variant="destructive">{t('agentFiles.badge.drifted')}</Badge>}
          {readOnlyFile ? (
            <Badge variant="outline">{t('agentFiles.badge.readOnly')}</Badge>
          ) : (
            !entry.editable_by_me && <Badge variant="ghost">{t('agentFiles.badge.viewOnly')}</Badge>
          )}
        </span>
      </button>
    </li>
  );
}

/**
 * The left column of the profile file tab: the fixed files in plain
 * language, then this employee's own skills. File names stay small and grey.
 */
export function AgentFileList({ files, selected, onSelect }: AgentFileListProps) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const fixed = files.filter((f) => !isSkillKey(f.file));
  const skills = files.filter((f) => isSkillKey(f.file));
  return (
    <nav aria-label={t('agentFiles.list.label')} className="space-y-4">
      <ul className="space-y-0.5">
        {fixed.map((f) => (
          <Item key={f.file} entry={f} selected={f.file === selected} onSelect={() => onSelect(f.file)} />
        ))}
      </ul>
      <div className="space-y-1">
        <p className="px-3 text-xs font-medium text-muted-foreground">{t('agentFiles.list.skills')}</p>
        {skills.length === 0 ? (
          <p className="px-3 text-xs text-muted-foreground">{t('agentFiles.list.noSkills')}</p>
        ) : (
          <ul className="space-y-0.5">
            {skills.map((f) => (
              <Item key={f.file} entry={f} selected={f.file === selected} onSelect={() => onSelect(f.file)} />
            ))}
          </ul>
        )}
      </div>
    </nav>
  );
}
