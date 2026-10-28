import { useEffect, useId, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useNavigate } from 'react-router';
import { Search } from 'lucide-react';

import { api } from '@/lib/api';
import {
  assignableSkills,
  formatRunTime,
  groupSkills,
  matchesSkill,
  type AssignableSkill,
} from '@/lib/skill-assign';
import { toast, formatError } from '@/lib/toast';
import { cn } from '@/lib/utils';
import { Input, Skeleton } from '@/components/mds';

import { AssignFooter } from './AssignFooter';
import { AttachmentChips, AttachmentDropZone, toAttachmentInput, useTaskAttachments } from './TaskAttachments';
import { announceGoalCreated, assignErrorText, createdGoalPath } from './announce';

/**
 * 一般 — pick a skill, optionally add one line and some files, hand it over.
 *
 * The list comes from the aggregate `skills.list`; only skills at least one
 * employee has switched on are shown, 「最近執行」 first (newest first), then
 * 「其他技能」 (by name). Who runs it is decided by the gateway
 * (`tasks.skill_assign`); the footer line predicts it with the same rule
 * (`lib/skill-assign`).
 */

function SkillRow({
  skill,
  checked,
  onSelect,
  name,
}: {
  skill: AssignableSkill;
  checked: boolean;
  onSelect: () => void;
  name: string;
}) {
  const intl = useIntl();
  const when = skill.runner.lastUsedAt ? formatRunTime(skill.runner.lastUsedAt) : null;
  const meta = when
    ? `${when} · ${skill.runner.displayName}`
    : intl.formatMessage({ id: 'assign.skill.neverRun' });
  return (
    <label
      className={cn(
        'flex min-h-16 cursor-pointer items-center gap-3 rounded-xl border px-3 py-2.5 transition-colors motion-reduce:transition-none',
        'has-[:focus-visible]:ring-3 has-[:focus-visible]:ring-ring/50',
        checked
          ? 'border-brand bg-brand/5 ring-1 ring-brand'
          : 'border-surface-border hover:bg-muted/40',
      )}
    >
      <input
        type="radio"
        name={name}
        value={skill.name}
        checked={checked}
        onChange={onSelect}
        className="sr-only"
      />
      <span
        aria-hidden
        className={cn(
          'grid size-4 shrink-0 place-items-center rounded-full border-2',
          checked ? 'border-brand' : 'border-muted-foreground/50',
        )}
      >
        {checked && <span className="size-2 rounded-full bg-brand" />}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium text-foreground">{skill.displayName}</span>
        {skill.description && (
          <span className="block truncate text-xs text-muted-foreground">{skill.description}</span>
        )}
      </span>
      <span className="shrink-0 text-right text-xs text-muted-foreground">{meta}</span>
    </label>
  );
}

function SkillGroup({
  heading,
  skills,
  selected,
  onSelect,
  name,
}: {
  heading: string;
  skills: readonly AssignableSkill[];
  selected: string | null;
  onSelect: (skill: string) => void;
  name: string;
}) {
  const headingId = useId();
  if (skills.length === 0) return null;
  return (
    <section className="space-y-2">
      <h3 id={headingId} className="text-xs font-medium text-muted-foreground">
        {heading}
      </h3>
      <div role="radiogroup" aria-labelledby={headingId} className="space-y-2">
        {skills.map((s) => (
          <SkillRow
            key={s.name}
            skill={s}
            name={name}
            checked={selected === s.name}
            onSelect={() => onSelect(s.name)}
          />
        ))}
      </div>
    </section>
  );
}

export function SkillAssignTab({ onCancel }: { onCancel: () => void }) {
  const intl = useIntl();
  const navigate = useNavigate();
  const radioName = useId();
  const [skills, setSkills] = useState<AssignableSkill[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<string | null>(null);
  const [note, setNote] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const attach = useTaskAttachments();

  useEffect(() => {
    let alive = true;
    api.skills
      .listAll()
      .then((list) => {
        if (alive) setSkills(assignableSkills(list));
      })
      .catch((e) => {
        if (alive) setLoadError(formatError(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  const groups = useMemo(
    () => groupSkills((skills ?? []).filter((s) => matchesSkill(s, query)), intl.locale),
    [skills, query, intl.locale],
  );
  const chosen = skills?.find((s) => s.name === selected) ?? null;

  const submit = async () => {
    if (!chosen || submitting) return;
    setSubmitting(true);
    try {
      const result = await api.tasks.skillAssign({
        skill_name: chosen.name,
        note: note.trim() || undefined,
        attachments: toAttachmentInput(attach.files),
      });
      announceGoalCreated(intl, result);
      navigate(createdGoalPath(result));
    } catch (e) {
      toast.error(assignErrorText(intl, e));
    } finally {
      setSubmitting(false);
    }
  };

  const runnerLine = chosen
    ? intl.formatMessage(
        { id: chosen.runner.lastUsedAt ? 'assign.skill.runner' : 'assign.skill.runnerFirst' },
        { name: <strong className="font-medium text-foreground">{chosen.runner.displayName}</strong> },
      )
    : intl.formatMessage({ id: 'assign.skill.pickFirst' });

  const noneAvailable = skills !== null && skills.length === 0;
  const noMatch = skills !== null && skills.length > 0 && groups.recent.length + groups.other.length === 0;

  return (
    <div>
      <div className="space-y-5">
        {!noneAvailable && !loadError && (
          <div className="relative">
            <label htmlFor="assign-skill-search" className="sr-only">
              {intl.formatMessage({ id: 'assign.skill.search' })}
            </label>
            <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
            <Input
              id="assign-skill-search"
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={intl.formatMessage({ id: 'assign.skill.search' })}
              className="h-11 pl-9 sm:h-9"
            />
          </div>
        )}

        {loadError ? (
          <p role="alert" className="rounded-xl border border-surface-border px-4 py-6 text-center text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'assign.skill.loadFailed' }, { message: loadError })}
          </p>
        ) : skills === null ? (
          <div className="space-y-2" aria-busy="true">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-16 w-full rounded-xl" />
            ))}
          </div>
        ) : noneAvailable ? (
          <div className="rounded-xl border border-dashed border-surface-border px-4 py-8 text-center">
            <p className="text-sm text-foreground">{intl.formatMessage({ id: 'assign.skill.empty' })}</p>
            <Link
              to="/skills"
              className="mt-2 inline-flex min-h-[44px] items-center text-sm font-medium text-brand underline-offset-2 outline-none hover:underline focus-visible:ring-3 focus-visible:ring-ring/50 sm:min-h-0"
            >
              {intl.formatMessage({ id: 'assign.skill.emptyLink' })}
            </Link>
          </div>
        ) : noMatch ? (
          <p className="px-1 py-4 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'assign.skill.noMatch' })}
          </p>
        ) : (
          <div className="space-y-5">
            <SkillGroup
              heading={intl.formatMessage({ id: 'assign.skill.recent' })}
              skills={groups.recent}
              selected={selected}
              onSelect={setSelected}
              name={radioName}
            />
            <SkillGroup
              heading={intl.formatMessage({ id: 'assign.skill.other' })}
              skills={groups.other}
              selected={selected}
              onSelect={setSelected}
              name={radioName}
            />
          </div>
        )}

        {!noneAvailable && (
          <>
            <div className="space-y-1.5">
              <label htmlFor="assign-skill-note" className="text-xs font-medium text-muted-foreground">
                {intl.formatMessage({ id: 'assign.skill.note' })}
              </label>
              <Input
                id="assign-skill-note"
                value={note}
                maxLength={500}
                onChange={(e) => setNote(e.target.value)}
                placeholder={intl.formatMessage({ id: 'assign.skill.notePlaceholder' })}
                className="h-11 sm:h-9"
                disabled={submitting}
              />
            </div>

            <div className="space-y-2">
              <AttachmentDropZone
                onFiles={(f) => void attach.addFiles(f)}
                hint={intl.formatMessage({ id: 'assign.attach.skillHint' })}
                disabled={submitting}
              />
              <AttachmentChips files={attach.files} onRemove={attach.remove} disabled={submitting} />
            </div>
          </>
        )}
      </div>

      <AssignFooter
        note={<span aria-live="polite">{runnerLine}</span>}
        submitLabel={intl.formatMessage({ id: 'assign.submit' })}
        onCancel={onCancel}
        onSubmit={() => void submit()}
        canSubmit={chosen !== null}
        submitting={submitting}
      />
    </div>
  );
}
