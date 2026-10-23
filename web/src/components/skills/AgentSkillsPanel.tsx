import { useState, useEffect, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Archive, RotateCcw, FileText } from 'lucide-react';
import { api, type SkillInfo, type SkillStateStatus } from '@/lib/api';
import { skillsManageApi, type ArchivedSkill, type SkillTarget } from '@/lib/api-skills-manage';
import { sortSkills } from '@/lib/skill-library';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button, Switch, buttonVariants } from '@/components/mds';
import { useSkillActions } from './use-skill-actions';
import { useLastUsedLabel } from './SkillUsageDialog';
import { RemoveSkillsDialog } from './RemoveSkillsDialog';
import { SkillStateErrors } from './SkillStateErrors';
import { isRemovable, sortFieldsOf } from './skill-rows';

/**
 * One employee's skills on the employee settings page (B8): every skill the
 * employee loads, with a switch, last use, and removal for its own skills;
 * removed skills can be restored. Read for Viewer, change for Owner / admin
 * (the gateway decides; the switches are disabled when it says no).
 */
export function AgentSkillsPanel({ agentId, agentName }: { agentId: string; agentName: string }) {
  const intl = useIntl();
  const lastUsed = useLastUsedLabel();
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [state, setState] = useState<SkillStateStatus | undefined>();
  const [canManage, setCanManage] = useState(false);
  const [archived, setArchived] = useState<ArchivedSkill[]>([]);
  const [canRestore, setCanRestore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [removeTargets, setRemoveTargets] = useState<SkillTarget[] | null>(null);

  const load = useCallback(async () => {
    try {
      const [list, arch] = await Promise.all([
        api.skills.list(agentId),
        skillsManageApi.archived(agentId).catch(() => null),
      ]);
      setSkills(list?.skills ?? []);
      setState(list?.skill_state);
      setCanManage(list?.can_manage === true);
      setArchived(arch?.archived ?? []);
      setCanRestore(arch?.can_restore === true);
      setError(null);
    } catch (e) {
      setError(formatError(e));
    }
  }, [agentId]);

  useEffect(() => {
    void load();
  }, [load]);

  const actions = useSkillActions(() => void load());
  const sorted = useMemo(
    () => sortSkills(skills, 'name', intl.locale, (s) => sortFieldsOf(s)),
    [skills, intl.locale],
  );

  const restore = useCallback(
    async (a: ArchivedSkill) => {
      try {
        await skillsManageApi.restore(agentId, a.archive_id);
        toast.success(intl.formatMessage({ id: 'skills.manage.restoredDone' }, { name: a.skill_name }));
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        void load();
      }
    },
    [agentId, intl, load],
  );

  if (error) {
    return <p className="font-mono text-xs break-all text-destructive">{error}</p>;
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <p className="min-w-0 flex-1 text-xs text-muted-foreground">{intl.formatMessage({ id: 'skills.manage.panelHint' })}</p>
        {/* Editing a skill's text lives on the employee's profile files tab. */}
        <Link
          to={`/agents/${encodeURIComponent(agentId)}/files`}
          className={buttonVariants({ variant: 'outline', size: 'sm' })}
        >
          <FileText />
          {intl.formatMessage({ id: 'skills.manage.editFiles' })}
        </Link>
      </div>
      {state && <SkillStateErrors states={{ [agentId]: state }} nameOf={() => agentName} />}
      {sorted.length === 0 ? (
        <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'skills.my.empty' })}</p>
      ) : (
        <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
          {sorted.map((s) => (
            <li key={`${s.scope}:${s.name}`} className="flex items-center gap-3 px-3 py-2">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm text-foreground" title={s.name}>{s.display_name || s.name}</span>
                  <Badge variant="secondary">{intl.formatMessage({ id: `skills.my.scope.${s.scope ?? 'agent'}` })}</Badge>
                </div>
                <p className="truncate text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'skills.usage.line' }, { count: s.use_count ?? 0, when: lastUsed(s.last_used_at) })}
                </p>
              </div>
              {isRemovable(s) && (
                <Button
                  variant="ghost"
                  size="icon-sm"
                  disabled={!canManage || actions.busy}
                  onClick={() => setRemoveTargets([{ agent_id: agentId, skill_name: s.name }])}
                  aria-label={intl.formatMessage({ id: 'skills.manage.removeOne' }, { name: s.name })}
                >
                  <Archive />
                </Button>
              )}
              <Switch
                checked={s.enabled !== false}
                disabled={!canManage || actions.busy}
                onCheckedChange={(on) => void actions.setEnabled([{ agent_id: agentId, skill_name: s.name }], on)}
                aria-label={intl.formatMessage({ id: 'skills.manage.switchFor' }, { agent: agentName, name: s.name })}
              />
            </li>
          ))}
        </ul>
      )}
      {archived.length > 0 && (
        <div className="space-y-1.5">
          <h4 className="text-xs font-medium text-muted-foreground">{intl.formatMessage({ id: 'skills.manage.archived' })}</h4>
          <ul className="space-y-1">
            {archived.map((a) => (
              <li key={a.archive_id} className="flex items-center justify-between gap-2 text-sm">
                <span className="truncate text-foreground">{a.skill_name}</span>
                <span className="flex items-center gap-2">
                  <span className="text-xs text-muted-foreground">{lastUsed(a.archived_at)}</span>
                  <Button variant="outline" size="sm" disabled={!canRestore} onClick={() => void restore(a)}>
                    <RotateCcw />
                    {intl.formatMessage({ id: 'skills.manage.restore' })}
                  </Button>
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {removeTargets && (
        <RemoveSkillsDialog
          targets={removeTargets}
          nameOf={() => agentName}
          busy={actions.busy}
          onConfirm={() => void actions.remove(removeTargets).then(() => setRemoveTargets(null))}
          onClose={() => setRemoveTargets(null)}
        />
      )}
    </div>
  );
}
