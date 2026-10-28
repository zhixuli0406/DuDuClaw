import type { SkillInfo, SkillsListAll } from './api';

/**
 * The 交辦任務 page's 「一般」 tab: which skills can be handed over, and who
 * would run each one.
 *
 * Built from the aggregate `skills.list` answer. The runner shown here mirrors
 * the gateway rule in `tasks_skill_assign_rpc.rs` exactly (the gateway decides
 * for real; this only predicts it for the 「由 X 執行」 line):
 * among holders that have the skill switched on and whose copy did not fail
 * the install scan, the one with the latest `last_used_at`; if none ran it,
 * the first by employee id. Timestamps compare as strings, as on the server.
 */

export interface SkillRunner {
  readonly agentId: string;
  readonly displayName: string;
  readonly lastUsedAt: string | null;
}

export interface AssignableSkill {
  readonly name: string;
  readonly displayName: string;
  readonly description: string;
  readonly runner: SkillRunner;
}

export interface SkillGroups {
  /** Ran at least once — newest first. */
  readonly recent: readonly AssignableSkill[];
  /** Never ran — by display name. */
  readonly other: readonly AssignableSkill[];
}

interface Holding {
  agentId: string;
  displayName: string;
  eligible: boolean;
  lastUsedAt: string | null;
}

interface Collected {
  name: string;
  displayName: string;
  description: string;
  holdings: Holding[];
}

function text(v: string | null | undefined): string {
  return typeof v === 'string' ? v.trim() : '';
}

function meta(map: Map<string, Collected>, s: SkillInfo): Collected {
  let c = map.get(s.name);
  if (!c) {
    c = { name: s.name, displayName: text(s.display_name), description: text(s.description), holdings: [] };
    map.set(s.name, c);
  }
  return c;
}

/** Plain code-unit order, the same as Rust's `String` ordering for ASCII ids. */
function byId(a: Holding, b: Holding): number {
  return a.agentId < b.agentId ? -1 : a.agentId > b.agentId ? 1 : 0;
}

/** The gateway's runner rule over one skill's holders. */
export function pickRunner(holdings: readonly Holding[]): Holding | null {
  const eligible = holdings.filter((h) => h.eligible).sort(byId);
  let best: Holding | null = null;
  for (const h of eligible) {
    if (!h.lastUsedAt) continue;
    if (!best || !best.lastUsedAt || h.lastUsedAt > best.lastUsedAt) best = h;
  }
  return best ?? eligible[0] ?? null;
}

/** Every skill at least one employee may run, with its predicted runner. */
export function assignableSkills(list: SkillsListAll | null | undefined): AssignableSkill[] {
  if (!list) return [];
  const map = new Map<string, Collected>();
  for (const g of list.global_skills ?? []) {
    const c = meta(map, g);
    const failed = g.security_status === 'fail';
    for (const h of g.holders ?? []) {
      c.holdings.push({
        agentId: h.agent_id,
        displayName: text(h.display_name) || h.agent_id,
        eligible: h.enabled === true && !failed,
        lastUsedAt: h.last_used_at ?? null,
      });
    }
  }
  for (const a of list.agents ?? []) {
    for (const s of a.skills ?? []) {
      const c = meta(map, s);
      if (!c.displayName) c.displayName = text(s.display_name);
      if (!c.description) c.description = text(s.description);
      c.holdings.push({
        agentId: a.agent_id,
        displayName: text(a.display_name) || a.agent_id,
        eligible: s.enabled === true && s.security_status !== 'fail',
        lastUsedAt: s.last_used_at ?? null,
      });
    }
  }
  const out: AssignableSkill[] = [];
  for (const c of map.values()) {
    const r = pickRunner(c.holdings);
    if (!r) continue;
    out.push({
      name: c.name,
      displayName: c.displayName || c.name,
      description: c.description,
      runner: { agentId: r.agentId, displayName: r.displayName, lastUsedAt: r.lastUsedAt },
    });
  }
  return out;
}

/** Split into 最近執行 (newest first) and 其他技能 (by display name). */
export function groupSkills(skills: readonly AssignableSkill[], locale: string): SkillGroups {
  const recent = skills
    .filter((s) => s.runner.lastUsedAt)
    .sort((a, b) => ((a.runner.lastUsedAt ?? '') < (b.runner.lastUsedAt ?? '') ? 1 : -1));
  const other = skills
    .filter((s) => !s.runner.lastUsedAt)
    .sort((a, b) => a.displayName.localeCompare(b.displayName, locale));
  return { recent, other };
}

/** Case-insensitive match on display name, id and description. */
export function matchesSkill(s: AssignableSkill, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [s.displayName, s.name, s.description].some((v) => v.toLowerCase().includes(q));
}

/** `MM/DD HH:mm` in the viewer's time zone; `null` for an unreadable time. */
export function formatRunTime(iso: string): string | null {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
