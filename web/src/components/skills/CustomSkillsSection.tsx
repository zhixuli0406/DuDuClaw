import { useState, useEffect, useCallback, useMemo } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Link } from 'react-router';
import { Plus, Puzzle } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  Button,
  Badge,
  CollectionPageState,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  ActorAvatar,
} from '@/components/mds';
import { toast, formatError } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import { listCustomSkills, type CustomSkillRecord } from '@/lib/api-custom-skills';
import { statusMeta, statusToneBadge, statusToneDot, formatTimeSaved } from './status-meta';

/**
 * CustomSkillsSection — the "自建技能" block inside the "我的技能" tab (T13.2),
 * re-skinned onto the MDS ListGrid (spec §4). Lists the current user's custom
 * skills as single-row entries (name link + status dot/badge + time estimate +
 * builder + updated), with a "＋自建技能" CTA into the `/skills/new` wizard.
 * The optional `filter` narrows by display name (shares the page search box).
 */
const COLUMNS = 'minmax(0,1fr) auto auto auto auto';

export function CustomSkillsSection({ filter = '' }: { filter?: string }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const navigate = useNavigate();

  const [skills, setSkills] = useState<CustomSkillRecord[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const res = await listCustomSkills();
      setSkills(res?.custom_skills ?? []);
    } catch (e) {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      setSkills([]);
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    load();
  }, [load]);

  const q = filter.trim().toLowerCase();
  const visible = useMemo(
    () => (q ? skills.filter((s) => s.display_name.toLowerCase().includes(q)) : skills),
    [skills, q],
  );

  const newButton = (
    <Button variant="brand" size="sm" onClick={() => navigate('/skills/new')}>
      <Plus />
      {t('skills.custom.new')}
    </Button>
  );

  return (
    <section className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-sm font-medium text-foreground">{t('skills.custom.sectionTitle')}</h2>
        {newButton}
      </div>

      {loading ? (
        <CollectionPageState state="loading" />
      ) : visible.length === 0 ? (
        <CollectionPageState
          state="empty"
          icon={Puzzle}
          title={t(q ? 'skills.market.noResults' : 'skills.custom.empty')}
          description={q ? undefined : t('skills.custom.emptyHint')}
          action={q ? undefined : newButton}
        />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={COLUMNS}
            className="!h-auto [&>[aria-hidden]]:hidden"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{t('skills.custom.col.name')}</ListGridHeaderCell>
                <ListGridHeaderCell>{t('skills.custom.col.status')}</ListGridHeaderCell>
                <ListGridHeaderCell hideBelow className="justify-end">
                  {t('skills.custom.col.timeSaved')}
                </ListGridHeaderCell>
                <ListGridHeaderCell hideBelow>{t('skills.custom.col.builtBy')}</ListGridHeaderCell>
                <ListGridHeaderCell hideBelow className="justify-end">
                  {t('skills.custom.col.updated')}
                </ListGridHeaderCell>
              </ListGridHeader>
            }
          >
            {visible.map((s) => (
              <CustomSkillRow key={s.id} skill={s} />
            ))}
          </ListGridContainer>
        </div>
      )}
    </section>
  );
}

function CustomSkillRow({ skill }: { skill: CustomSkillRecord }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const meta = statusMeta(skill.status);
  const badge = statusToneBadge(meta.tone);
  const to = `/skills/custom/${skill.id}`;

  return (
    <ListGridRow to={to}>
      <ListGridCell className="gap-2">
        <Puzzle className="size-4 shrink-0 text-muted-foreground" />
        <Link to={to} className="truncate text-sm font-medium text-foreground hover:underline" title={skill.display_name}>
          {skill.display_name}
        </Link>
      </ListGridCell>
      <ListGridCell>
        <span className={cn('mr-2 size-1.5 shrink-0 rounded-full', statusToneDot(meta.tone))} />
        <Badge variant={badge.variant} className={badge.className}>
          {t(meta.labelKey)}
        </Badge>
      </ListGridCell>
      <ListGridCell hideBelow className="justify-end">
        <span className="truncate font-mono text-xs tabular-nums text-muted-foreground">
          {formatTimeSaved(intl, skill.time_saved_value, skill.time_saved_unit)}
        </span>
      </ListGridCell>
      <ListGridCell hideBelow>
        {skill.built_by_agent ? (
          <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
            <ActorAvatar actorType="agent" size="xs" name={skill.built_by_agent} />
            <span className="truncate">{skill.built_by_agent}</span>
          </span>
        ) : (
          <span className="text-xs text-muted-foreground">{skill.created_by_user || '—'}</span>
        )}
      </ListGridCell>
      <ListGridCell hideBelow className="justify-end">
        <span className="font-mono text-xs tabular-nums text-muted-foreground">{timeAgo(skill.created_at)}</span>
      </ListGridCell>
    </ListGridRow>
  );
}
