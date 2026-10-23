import { useIntl } from 'react-intl';
import type { ExpertAgentEdits } from '@/lib/api';

/** Employee files people recognise by name, shown with their label. */
const FILE_LABELS: Record<string, string> = {
  'SOUL.md': 'soul',
  'IDENTITY.md': 'identity',
  'INSTRUCTIONS.md': 'instructions',
  'CONTRACT.toml': 'contract',
  'agent.toml': 'settings',
};

/**
 * EditsList — what changed on each imported employee since the import:
 * the changes a replace would move to the backup. Known files carry a
 * plain label ("人格設定"); everything else shows its file name.
 */
export function EditsList({ edits }: { edits: ExpertAgentEdits[] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const fileLabel = (path: string) => {
    const label = FILE_LABELS[path];
    return label ? `${t(`experts.reimport.file.${label}`)}（${path}）` : path;
  };

  return (
    <ul className="space-y-2" aria-label={t('experts.reimport.edits.title')}>
      {edits.map((e) => (
        <li key={e.agent} className="rounded-lg border border-surface-border bg-background/60 px-2.5 py-2">
          <p className="font-medium">{e.display || e.agent}</p>
          {e.baseline === 'time' && (
            <p className="text-muted-foreground">{t('experts.reimport.edits.byTime')}</p>
          )}
          <ul className="mt-1 space-y-0.5">
            {e.changes.map((c, i) => (
              <li key={`${c.path}-${i}`} className="flex gap-1.5 break-all">
                <span className="shrink-0 text-muted-foreground">{t(`experts.reimport.change.${c.change}`)}</span>
                {c.path && <span>{fileLabel(c.path)}</span>}
              </li>
            ))}
            {e.more > 0 && (
              <li className="text-muted-foreground">{t('experts.reimport.edits.more', { count: e.more })}</li>
            )}
          </ul>
        </li>
      ))}
    </ul>
  );
}
