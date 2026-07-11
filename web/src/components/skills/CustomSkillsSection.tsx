import { useState, useEffect, useCallback } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { Plus, Puzzle, Clock } from 'lucide-react';
import {
  Section,
  Card,
  Button,
  Badge,
  EmptyState,
  CharacterAvatar,
} from '@/components/ui';
import { toast, formatError } from '@/lib/toast';
import { timeAgo } from '@/lib/format';
import { listCustomSkills, type CustomSkillRecord } from '@/lib/api-custom-skills';
import { statusMeta, formatTimeSaved } from './status-meta';

/**
 * CustomSkillsSection — the "自建技能" block inside the "我的技能" tab (T13.2).
 * Minimal-intrusion: mounted at one place in SkillMarketPage. Lists the current
 * user's custom skills with a status chip + time estimate + builder, and a
 * "＋自建技能" CTA into the `/skills/new` wizard. Rows open the detail page.
 */
export function CustomSkillsSection() {
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

  return (
    <Section
      title={t('skills.custom.sectionTitle')}
      actions={
        <Button variant="primary" size="sm" icon={Plus} onClick={() => navigate('/skills/new')}>
          {t('skills.custom.new')}
        </Button>
      }
    >
      {loading ? (
        <div className="py-10 text-center text-stone-400">{t('common.loading')}</div>
      ) : skills.length === 0 ? (
        <Card>
          <EmptyState
            icon={Puzzle}
            title={t('skills.custom.empty')}
            hint={t('skills.custom.emptyHint')}
            action={
              <Button variant="primary" size="sm" icon={Plus} onClick={() => navigate('/skills/new')}>
                {t('skills.custom.new')}
              </Button>
            }
          />
        </Card>
      ) : (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {skills.map((s) => (
            <CustomSkillCard key={s.id} skill={s} onOpen={() => navigate(`/skills/custom/${s.id}`)} />
          ))}
        </div>
      )}
    </Section>
  );
}

function CustomSkillCard({ skill, onOpen }: { skill: CustomSkillRecord; onOpen: () => void }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const meta = statusMeta(skill.status);
  return (
    <Card interactive onClick={onOpen}>
      <div className="flex items-start justify-between gap-2">
        <h3 className="min-w-0 truncate font-semibold text-stone-900 dark:text-stone-50" title={skill.display_name}>
          {skill.display_name}
        </h3>
        <Badge tone={meta.tone}>{t(meta.labelKey)}</Badge>
      </div>

      <p className="mt-1 flex items-center gap-1.5 text-sm text-amber-600 dark:text-amber-400">
        <Clock className="h-3.5 w-3.5" />
        {formatTimeSaved(intl, skill.time_saved_value, skill.time_saved_unit)}
      </p>

      <div className="mt-4 flex items-center justify-between gap-2 border-t border-[var(--panel-border)] pt-3 text-xs text-stone-500 dark:text-stone-400">
        {skill.built_by_agent ? (
          <span className="flex min-w-0 items-center gap-1.5">
            <CharacterAvatar agentId={skill.built_by_agent} name={skill.built_by_agent} size={16} />
            <span className="truncate">{skill.built_by_agent}</span>
          </span>
        ) : (
          <span>{skill.created_by_user || '—'}</span>
        )}
        <span className="shrink-0">{timeAgo(skill.created_at)}</span>
      </div>
    </Card>
  );
}
