/**
 * `@` autocomplete for the chat composer: typing `@` lists the AI staff the
 * signed-in account may call (the `agents.list` result is already filtered
 * to that set by the gateway), filters as you type and inserts `@name `.
 *
 * Mirrors the gateway matcher (`duduclaw_agent::trigger_match`): a mention
 * starts at `@` or `＠` that is not glued to an e-mail local part, and the
 * inserted name is the display name when it is unique, else the staff id.
 */

export interface MentionAgent {
  readonly name: string;
  readonly display_name: string;
  readonly trigger?: string;
  readonly icon?: string;
  readonly department?: string;
  readonly role?: string;
  readonly status?: string;
  readonly archived?: boolean;
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

/** The text that calls exactly `agent`: its display name when no other
 *  callable staff shares it, else its id. */
export function mentionText(agent: MentionAgent, all: readonly MentionAgent[]): string {
  const display = agent.display_name.trim();
  const shared = all.filter((a) => a.display_name.trim().toLocaleLowerCase() === display.toLocaleLowerCase()).length;
  return display && shared <= 1 ? `@${display}` : `@${agent.name}`;
}

/** Replace the active mention with `call` plus a space; returns the new text
 *  and the caret position after the insertion. */
export function applyMention(text: string, mention: ActiveMention, call: string): { text: string; caret: number } {
  const before = text.slice(0, mention.start);
  const after = text.slice(mention.end).replace(/^\s+/, '');
  const inserted = `${call} `;
  return { text: `${before}${inserted}${after}`, caret: before.length + inserted.length };
}
