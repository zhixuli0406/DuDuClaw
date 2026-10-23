/**
 * Row identity and targets for the installed-skill list (B8).
 *
 * A skill name alone does not identify a row: two employees can each have a
 * skill called `quote`, and the company-wide `tone` is held by every
 * employee. Rows are keyed by layer + owner + name, and an action on a row
 * resolves to the exact (employee, skill) pairs it affects.
 */
import type { SkillInfo } from '@/lib/api';
import type { SkillTarget } from '@/lib/api-skills-manage';
import type { SortFields } from '@/lib/skill-library';

/** Unique key for a row: `<scope>:<agent>:<name>`. */
export function rowKey(skill: SkillInfo): string {
  return `${skill.scope ?? 'agent'}:${skill.agent_id ?? ''}:${skill.name}`;
}

/** Sorting fields for a row; `ownerName` disambiguates same-named rows. */
export function sortFieldsOf(skill: SkillInfo, ownerName = ''): SortFields {
  return {
    name: skill.display_name || skill.name,
    lastUsedAt: skill.last_used_at ?? null,
    useCount: skill.use_count ?? 0,
    addedAt: skill.modified_at != null ? skill.modified_at * 1000 : null,
    stableKey: `${rowKey(skill)}:${ownerName}`,
  };
}

/** Searchable text of a row. */
export function searchFieldsOf(skill: SkillInfo, ownerName = ''): string[] {
  return [
    skill.name,
    skill.display_name ?? '',
    skill.description ?? '',
    ...(skill.tags ?? []),
    ownerName,
  ];
}

/**
 * The (employee, skill) pairs a switch on this row changes: the row's own
 * employee, or — for a company-wide row in the all-employees view — every
 * employee holding it.
 */
export function switchTargetsOf(skill: SkillInfo): SkillTarget[] {
  if (skill.agent_id) return [{ agent_id: skill.agent_id, skill_name: skill.name }];
  return (skill.holders ?? []).map((h) => ({ agent_id: h.agent_id, skill_name: skill.name }));
}

/** Only an employee's own skills can be removed (company-wide and department ones are shared). */
export function isRemovable(skill: SkillInfo): boolean {
  return skill.scope === 'agent' && !!skill.agent_id;
}

/** Whether the row counts as switched on (a company-wide row: for anyone). */
export function isEnabled(skill: SkillInfo): boolean {
  return skill.enabled !== false;
}
