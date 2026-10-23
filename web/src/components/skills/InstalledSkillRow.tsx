import { useIntl } from 'react-intl';
import {
  Sparkles,
  MoreHorizontal,
  CheckCircle,
  Share2,
  Users,
  Power,
  PowerOff,
  Archive,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import type { SkillInfo } from '@/lib/api';
import {
  Badge,
  Button,
  Checkbox,
  ActorAvatar,
  ListGridRow,
  ListGridCell,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from '@/components/mds';
import { isEnabled, isRemovable } from './skill-rows';
import { useLastUsedLabel } from './SkillUsageDialog';

/** Security status → semantic dot. */
function securityDot(status?: string): string {
  if (status === 'pass') return 'bg-success';
  if (status === 'warn') return 'bg-warning';
  if (status === 'fail') return 'bg-destructive';
  return 'bg-muted-foreground';
}

export const MY_SKILLS_COLUMNS = 'minmax(11rem,1fr) auto auto auto 2.5rem';

/** One installed skill: name and description, owner, last use, security, actions. */
export function InstalledSkillRow({
  skill,
  owner,
  shared,
  canManage,
  selectable,
  selected,
  onSelect,
  onShare,
  onUsage,
  onToggle,
  onRemove,
}: {
  skill: SkillInfo;
  owner: string;
  shared: boolean;
  canManage: boolean;
  selectable: boolean;
  selected: boolean;
  onSelect: (v: boolean) => void;
  onShare: () => void;
  onUsage: () => void;
  onToggle: (enabled: boolean) => void;
  onRemove: () => void;
}) {
  const intl = useIntl();
  const lastUsed = useLastUsedLabel();
  const status = skill.security_status;
  const scope = skill.scope ?? 'agent';
  const enabled = isEnabled(skill);
  const holders = skill.holders;
  const label = skill.display_name || skill.name;
  return (
    <ListGridRow className={cn('cursor-default', !enabled && 'opacity-70')}>
      <ListGridCell className="gap-2">
        {selectable ? (
          <Checkbox
            checked={selected}
            onCheckedChange={(v) => onSelect(v === true)}
            aria-label={intl.formatMessage({ id: 'skills.manage.select' }, { name: label })}
          />
        ) : (
          <Sparkles className="size-4 shrink-0 text-muted-foreground" />
        )}
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-medium text-foreground" title={skill.name}>
              {label}
            </span>
            {!enabled && <Badge variant="outline">{intl.formatMessage({ id: 'skills.manage.off' })}</Badge>}
          </div>
          {skill.description && (
            <p className="truncate text-xs text-muted-foreground" title={skill.description}>
              {skill.description}
            </p>
          )}
        </div>
      </ListGridCell>
      <ListGridCell className="gap-2">
        <Badge variant="secondary" className="shrink-0 whitespace-nowrap">{intl.formatMessage({ id: `skills.my.scope.${scope}` })}</Badge>
        {holders ? (
          <span className="whitespace-nowrap text-xs text-muted-foreground">
            {intl.formatMessage(
              { id: 'skills.usage.holders' },
              { on: skill.enabled_count ?? holders.length, total: holders.length },
            )}
          </span>
        ) : (
          owner && (
            <>
              <ActorAvatar actorType="agent" size="xs" name={owner} />
              <span className="truncate text-xs text-muted-foreground" title={owner}>
                {owner}
              </span>
            </>
          )
        )}
      </ListGridCell>
      <ListGridCell>
        <span className="whitespace-nowrap text-xs text-muted-foreground" title={skill.last_used_at ?? undefined}>
          {intl.formatMessage(
            { id: 'skills.usage.line' },
            { count: skill.use_count ?? 0, when: lastUsed(skill.last_used_at) },
          )}
        </span>
      </ListGridCell>
      <ListGridCell>
        <span className={cn('mr-2 size-1.5 shrink-0 rounded-full', securityDot(status))} />
        <span className="truncate text-sm text-muted-foreground">
          {intl.formatMessage({ id: `skills.my.security.${status ?? 'unknown'}` })}
        </span>
      </ListGridCell>
      <ListGridCell className="justify-end">
        <DropdownMenu>
          <DropdownMenuTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={intl.formatMessage({ id: 'skills.my.moreActions' })}
                data-stop-row-nav
                onClick={(e) => e.stopPropagation()}
              />
            }
          >
            <MoreHorizontal />
          </DropdownMenuTrigger>
          <DropdownMenuContent>
            <DropdownMenuItem onClick={onUsage}>
              <Users />
              {intl.formatMessage({ id: 'skills.usage.open' })}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onToggle(!enabled)} disabled={!canManage}>
              {enabled ? <PowerOff /> : <Power />}
              {intl.formatMessage({
                id: holders
                  ? enabled
                    ? 'skills.manage.disableAll'
                    : 'skills.manage.enableAll'
                  : enabled
                    ? 'skills.manage.disable'
                    : 'skills.manage.enable',
              })}
            </DropdownMenuItem>
            {isRemovable(skill) && (
              <DropdownMenuItem onClick={onRemove} disabled={!canManage}>
                <Archive />
                {intl.formatMessage({ id: 'skills.manage.remove' })}
              </DropdownMenuItem>
            )}
            {/* A company-wide skill has no owning staffer to share from. */}
            <DropdownMenuItem onClick={onShare} disabled={shared || !skill.agent_id}>
              {shared ? <CheckCircle /> : <Share2 />}
              {intl.formatMessage({ id: shared ? 'skills.my.shared' : 'skills.shared.share' })}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </ListGridCell>
    </ListGridRow>
  );
}
