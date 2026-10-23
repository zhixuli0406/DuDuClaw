/**
 * `@` autocomplete for the chat composer: typing `@` lists the AI staff the
 * signed-in account may call (`agents.callable`: own staff, a manager's team,
 * the default and public staff — the same set the gateway lets through),
 * filters as you type and inserts `@name `.
 *
 * Mirrors the gateway matcher (`duduclaw_agent::trigger_match`): a mention
 * starts at `@` or `＠` that is not glued to an e-mail local part, and the
 * inserted name is the display name when it is unique, else the staff id.
 */

import { defaultLocale, messages, useLocaleStore } from '@/i18n';

export interface MentionAgent {
  readonly name: string;
  readonly display_name: string;
  readonly trigger?: string;
  readonly icon?: string;
  readonly department?: string;
  readonly role?: string;
  readonly status?: string;
  readonly archived?: boolean;
  /** From `agents.callable`: where the right to call comes from. */
  readonly via?: 'admin' | 'binding' | 'team' | 'default' | 'public';
  readonly via_user?: string;
  readonly via_user_name?: string;
  /** The server's exact `@…` text for this staff member. */
  readonly call_text?: string;
}

/** One section of the `@` popup. `team` sections are per report. */
export interface MentionGroup {
  readonly kind: 'mine' | 'team' | 'public';
  /** `team` only: the report's display name. */
  readonly owner?: string;
  readonly agents: readonly MentionAgent[];
}

/** An `@…` being typed: `start` is the index of the `@`, `query` the text
 *  between it and the caret. */
export interface ActiveMention {
  readonly start: number;
  readonly end: number;
  readonly query: string;
}

const MAX_QUERY = 32;

function isAt(ch: string): boolean {
  return ch === '@' || ch === '＠';
}

/** Characters that can end an e-mail local part (then `@` is not a mention). */
function isEmailChar(ch: string): boolean {
  return /[A-Za-z0-9._%+\-@]/.test(ch);
}

/** The mention under the caret, if the user is typing one. */
export function findActiveMention(text: string, caret: number): ActiveMention | null {
  const upto = text.slice(0, caret);
  for (let i = upto.length - 1; i >= 0 && upto.length - i <= MAX_QUERY + 1; i -= 1) {
    const ch = upto[i];
    if (/\s/.test(ch)) return null;
    if (isAt(ch)) {
      const prev = i > 0 ? upto[i - 1] : '';
      if (prev && isEmailChar(prev)) return null;
      return { start: i, end: caret, query: upto.slice(i + 1) };
    }
  }
  return null;
}

function names(a: MentionAgent): string[] {
  const trigger = (a.trigger ?? '').trim().replace(/^[@＠]/, '');
  return [a.display_name, trigger, a.name].filter((n) => n.trim().length > 0);
}

/** Staff that can be called right now (active, not archived). */
export function callableAgents(agents: readonly MentionAgent[]): MentionAgent[] {
  return agents.filter((a) => (a.status ?? 'active') === 'active' && !a.archived);
}

/** Candidates for `query`: names that start with it first, then names that
 *  contain it; case-insensitive. An empty query lists everyone. */
export function filterMentionCandidates(agents: readonly MentionAgent[], query: string): MentionAgent[] {
  const q = query.toLocaleLowerCase();
  const callable = callableAgents(agents);
  if (!q) return callable;
  const starts = callable.filter((a) => names(a).some((n) => n.toLocaleLowerCase().startsWith(q)));
  const contains = callable.filter(
    (a) => !starts.includes(a) && names(a).some((n) => n.toLocaleLowerCase().includes(q)),
  );
  return [...starts, ...contains];
}

/** The text that calls exactly `agent`: the server's `call_text` when it
 *  sent one, else its display name when no other callable staff shares it,
 *  else its id. */
export function mentionText(agent: MentionAgent, all: readonly MentionAgent[]): string {
  if (agent.call_text && agent.call_text.trim().length > 1) return agent.call_text.trim();
  const display = agent.display_name.trim();
  const shared = all.filter((a) => a.display_name.trim().toLocaleLowerCase() === display.toLocaleLowerCase()).length;
  return display && shared <= 1 ? `@${display}` : `@${agent.name}`;
}

/** Split candidates into the popup's sections, keeping their order: my own
 *  staff (and the default one), each report's staff (my team), public staff.
 *  The flat order of the result is the keyboard order. */
export function groupMentionCandidates(candidates: readonly MentionAgent[]): MentionGroup[] {
  const mine: MentionAgent[] = [];
  const publicOnes: MentionAgent[] = [];
  const team = new Map<string, MentionAgent[]>();
  for (const a of candidates) {
    if (a.via === 'team') {
      const owner = a.via_user_name || a.via_user || '';
      team.set(owner, [...(team.get(owner) ?? []), a]);
    } else if (a.via === 'public') {
      publicOnes.push(a);
    } else {
      mine.push(a);
    }
  }
  const groups: MentionGroup[] = [];
  if (mine.length) groups.push({ kind: 'mine', agents: mine });
  for (const [owner, agents] of team) groups.push({ kind: 'team', owner, agents });
  if (publicOnes.length) groups.push({ kind: 'public', agents: publicOnes });
  return groups;
}

/** Candidates in popup order (the order `groupMentionCandidates` renders). */
export function orderMentionCandidates(candidates: readonly MentionAgent[]): MentionAgent[] {
  return groupMentionCandidates(candidates).flatMap((g) => g.agents);
}

/** Replace the active mention with `call` plus a space; returns the new text
 *  and the caret position after the insertion. */
export function applyMention(text: string, mention: ActiveMention, call: string): { text: string; caret: number } {
  const before = text.slice(0, mention.start);
  const after = text.slice(mention.end).replace(/^\s+/, '');
  const inserted = `${call} `;
  return { text: `${before}${inserted}${after}`, caret: before.length + inserted.length };
}

/** Server error codes for `@` calls (see `webchat/route.rs`). */
export const MENTION_FORBIDDEN_CODE = 'mention_forbidden';
export const MENTION_CHOICES_CODE = 'mention_choices';

/** The viewer-language text for an `@` error frame, or `null` when the
 *  frame is not one (the caller then shows the server's message). */
export function mentionErrorText(
  frame: { readonly code?: unknown; readonly choices?: unknown },
  locale?: string,
): string | null {
  const lang = locale ?? useLocaleStore.getState().locale;
  const catalogue = messages[lang] ?? messages[defaultLocale];
  if (frame.code === MENTION_FORBIDDEN_CODE) {
    return catalogue['webchat.mention.forbidden'] ?? null;
  }
  if (frame.code === MENTION_CHOICES_CODE && Array.isArray(frame.choices)) {
    const names = frame.choices.filter((c): c is string => typeof c === 'string');
    if (names.length === 0) return catalogue['webchat.mention.none'] ?? null;
    const heading = catalogue['webchat.mention.choices'] ?? '';
    return [heading, ...names.map((n) => `• ${n}`)].join('\n');
  }
  return null;
}
