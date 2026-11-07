import type { OdooModelAccess } from '@/lib/api';

/** Button actions and other methods have no generic ACL: write is only a hint. */
export function actionAccess(access: OdooModelAccess | null | undefined, action: ActionGroup): boolean | null {
  return access?.[action === 'action' || action === 'execute' ? 'write' : action] ?? null;
}

/** Storage stays compatible with agent.toml; unknown tokens are never discarded. */
export const ACTION_GROUPS = ['read', 'create', 'write', 'unlink', 'action', 'execute'] as const;
export type ActionGroup = typeof ACTION_GROUPS[number];
export interface OdooRule { model: string; verbs: string[] }
export interface OdooPermissions {
  allowed_models: string[];
  allowed_actions: string[];
  unblock_models: string[];
}
const VERBS = new Set(['read', 'search', 'create', 'write', 'unlink', 'action', 'execute']);
export const validModel = (model: string) => /^[a-zA-Z0-9_.]{1,128}$/.test(model);

export function parseActions(entries: string[]) {
  const rules: OdooRule[] = [];
  const other: string[] = [];
  for (const entry of entries) {
    const [verb, model = '', ...extra] = entry.split(':');
    if (!VERBS.has(verb) || extra.length || (entry.includes(':') && !validModel(model))) {
      other.push(entry);
      continue;
    }
    let rule = rules.find((r) => r.model === model);
    if (!rule) { rule = { model, verbs: [] }; rules.push(rule); }
    if (!rule.verbs.includes(verb)) rule.verbs.push(verb);
  }
  return { rules, other };
}

export function serializeRules(rules: OdooRule[], other: string[], normalize = false) {
  const entries = rules.flatMap(({ model, verbs }) => {
    const next = normalize && (verbs.includes('read') || verbs.includes('search'))
      ? ['read', 'search', ...verbs.filter((v) => v !== 'read' && v !== 'search')]
      : verbs;
    return next.map((verb) => model ? `${verb}:${model}` : verb);
  });
  return [...new Set(entries), ...other];
}
export function normalizeActions(entries: string[]) {
  const { rules, other } = parseActions(entries);
  return serializeRules(rules, other, true);
}
export function partialRead(rule: OdooRule) {
  return rule.verbs.includes('read') !== rule.verbs.includes('search');
}
export function usedModels(value: Pick<OdooPermissions, 'allowed_models' | 'allowed_actions'>) {
  // Legacy/future verbs shown under Other settings can still reference a table.
  const scoped = value.allowed_actions.flatMap((entry) => {
    const parts = entry.split(':');
    return parts.length === 2 && validModel(parts[1]) ? [parts[1]] : [];
  });
  return new Set([...value.allowed_models, ...scoped]);
}
/** Only remove grants whose final explicit use was removed, preserving legacy standalone grants. */
export function reconcileUnblocks(before: OdooPermissions, next: OdooPermissions, blocked: string[]) {
  const wasUsed = usedModels(before);
  const nowUsed = usedModels(next);
  return next.unblock_models.filter((model) => !blocked.includes(model) || !wasUsed.has(model) || nowUsed.has(model));
}
