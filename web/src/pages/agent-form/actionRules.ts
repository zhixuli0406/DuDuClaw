// 2026-10 — `[capabilities] action_rules`: allow / ask / block per effect
// class, plus optional per-tool overrides. Pure helpers for the editor; the
// server validates the same shape (`capabilities_apply.rs`).

import type { ActionRule, ActionVerdict, ToolEffect } from '@/lib/api';

/** Every effect class, in the order the gateway lists them. */
export const TOOL_EFFECTS: ReadonlyArray<ToolEffect> = [
  'read', 'draft', 'send', 'publish', 'purchase', 'delete', 'modify', 'admin',
];

export const ACTION_VERDICTS: ReadonlyArray<ActionVerdict> = ['allow', 'ask', 'block'];

/** The verdict an effect rule sets, or `''` when there is none. When several
 *  rules name one effect (hand-written files) the strictest is shown, which is
 *  also what the gateway applies. */
export function effectVerdict(rules: ReadonlyArray<ActionRule>, effect: ToolEffect): ActionVerdict | '' {
  let best: ActionVerdict | '' = '';
  for (const r of rules) {
    if (r.effect === effect && rank(r.verdict) > rank(best)) best = r.verdict;
  }
  return best;
}

function rank(v: ActionVerdict | ''): number {
  return v === '' ? -1 : ACTION_VERDICTS.indexOf(v);
}

/** Replace every rule for `effect` with one rule (or none for `''`). Tool rules
 *  and other effects keep their order. */
export function setEffectVerdict(
  rules: ReadonlyArray<ActionRule>,
  effect: ToolEffect,
  verdict: ActionVerdict | '',
): ActionRule[] {
  const rest = rules.filter((r) => r.effect !== effect);
  if (verdict === '') return rest;
  const effectRules = TOOL_EFFECTS.flatMap((e) => {
    if (e === effect) return [{ effect, verdict }];
    return rest.filter((r) => r.effect === e);
  });
  return [...effectRules, ...rest.filter((r) => r.effect === undefined)];
}

/** The per-tool overrides, in file order. */
export function toolRules(rules: ReadonlyArray<ActionRule>): ActionRule[] {
  return rules.filter((r) => r.tool !== undefined);
}

/** Replace the per-tool overrides; effect rules are kept as they are. */
export function setToolRules(rules: ReadonlyArray<ActionRule>, tools: ReadonlyArray<ActionRule>): ActionRule[] {
  return [...rules.filter((r) => r.tool === undefined), ...tools];
}

/** Same check the server makes on a tool name before saving. */
export const ACTION_RULE_TOOL_RE = /^(mcp__[A-Za-z0-9_-]+__)?[A-Za-z0-9_]+\*?$/;
