import type { OdooLivechatConfig, OdooLivechatRule } from '@/lib/api';

/** Comma / 、 / newline separated text ⇄ trimmed, de-duplicated list. */
export function parseList(text: string): string[] {
  const out: string[] = [];
  for (const raw of text.split(/[,，、\n]/)) {
    const v = raw.trim();
    if (v && !out.includes(v)) out.push(v);
  }
  return out;
}

export function joinList(values: readonly (string | number)[]): string {
  return values.join(', ');
}

/** Positive integer ids from free text; anything else is dropped. */
export function parseIds(text: string): number[] {
  return parseList(text)
    .map((v) => Number(v))
    .filter((n) => Number.isInteger(n) && n > 0);
}

export function toggleId(list: readonly number[], id: number, on: boolean): number[] {
  const without = list.filter((x) => x !== id);
  return on ? [...without, id] : without;
}

export function emptyRule(): OdooLivechatRule {
  return {
    name: '',
    agent: '',
    enabled: true,
    livechat_channel_ids: [],
    chatbot_script_ids: [],
    country_codes: [],
    langs: [],
    visitor: 'any',
    partner_tags: [],
    keywords: [],
  };
}

export function moveItem<T>(list: readonly T[], from: number, to: number): T[] {
  if (to < 0 || to >= list.length) return [...list];
  const next = [...list];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

/** Mirrors the gateway's validation so most mistakes surface before Save. */
export function validateConfig(cfg: OdooLivechatConfig): string | null {
  for (const [i, r] of cfg.audience_rules.entries()) {
    if (!r.name.trim()) return `rule:${i + 1}:name`;
    if (!r.agent.trim()) return `rule:${i + 1}:agent`;
  }
  return null;
}

/** Whether a rule has no criteria at all (it then matches every visitor). */
export function isCatchAll(r: OdooLivechatRule): boolean {
  return (
    r.livechat_channel_ids.length === 0 &&
    r.chatbot_script_ids.length === 0 &&
    r.country_codes.length === 0 &&
    r.langs.length === 0 &&
    r.visitor === 'any' &&
    r.partner_tags.length === 0 &&
    r.keywords.length === 0
  );
}
