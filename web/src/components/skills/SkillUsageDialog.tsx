import { useIntl } from 'react-intl';
import {
  Badge,
  Switch,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  ActorAvatar,
} from '@/components/mds';

/** One employee that has the skill, as the usage dialog shows it. */
export interface HolderRow {
  agent_id: string;
  display_name: string;
  scope: 'global' | 'department' | 'agent';
  enabled: boolean;
  use_count: number;
  last_used_at?: string | null;
  can_manage: boolean;
}

/** "Last used" text for a timestamp, or the never-used label. */
export function useLastUsedLabel() {
  const intl = useIntl();
  return (iso?: string | null) => {
    if (!iso) return intl.formatMessage({ id: 'skills.usage.never' });
    const t = Date.parse(iso);
    return Number.isNaN(t)
      ? iso
      : intl.formatDate(t, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  };
}

/** Who has a skill, how often the system used it for each, and per-employee switches. */
export function SkillUsageDialog({
  skillName,
  description,
  holders,
  busy,
  onToggle,
  onClose,
}: {
  skillName: string;
  description?: string;
  holders: readonly HolderRow[];
  busy: boolean;
  onToggle: (agentId: string, enabled: boolean) => void;
  onClose: () => void;
}) {
  const intl = useIntl();
  const lastUsed = useLastUsedLabel();
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'skills.usage.title' }, { name: skillName })}</DialogTitle>
          <DialogDescription>
            {description || intl.formatMessage({ id: 'skills.usage.subtitle' })}
          </DialogDescription>
        </DialogHeader>
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'skills.usage.countHint' })}</p>
        {holders.length === 0 ? (
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'skills.usage.noHolders' })}</p>
        ) : (
          <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
            {holders.map((h) => (
              <li key={`${h.scope}:${h.agent_id}`} className="flex items-center gap-3 px-3 py-2">
                <ActorAvatar actorType="agent" size="xs" name={h.display_name} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-sm text-foreground">{h.display_name}</span>
                    <Badge variant="secondary">{intl.formatMessage({ id: `skills.my.scope.${h.scope}` })}</Badge>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage(
                      { id: 'skills.usage.line' },
                      { count: h.use_count, when: lastUsed(h.last_used_at) },
                    )}
                  </p>
                </div>
                <Switch
                  checked={h.enabled}
                  disabled={busy || !h.can_manage}
                  onCheckedChange={(v) => onToggle(h.agent_id, v)}
                  aria-label={intl.formatMessage(
                    { id: 'skills.manage.switchFor' },
                    { agent: h.display_name, name: skillName },
                  )}
                />
              </li>
            ))}
          </ul>
        )}
      </DialogContent>
    </Dialog>
  );
}
