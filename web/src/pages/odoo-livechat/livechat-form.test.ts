import { describe, it, expect } from 'vitest';
import { emptyRule, isCatchAll, moveItem, parseIds, parseList, toggleId, validateConfig } from './livechat-form';
import type { OdooLivechatConfig } from '@/lib/api';

const baseConfig = (): OdooLivechatConfig => ({
  enabled: true,
  bot_username: '',
  livechat_channel_ids: [],
  poll_interval_seconds: 4,
  default_agent: '',
  greeting: '',
  handoff_user_ids: [],
  handoff_keywords: [],
  handoff_message: '',
  audience_rules: [],
});

describe('odoo livechat form helpers', () => {
  it('parses lists split by commas, 、 and newlines, de-duplicated', () => {
    expect(parseList(' 退貨, 換貨、退貨\nrefund ,, ')).toEqual(['退貨', '換貨', 'refund']);
    expect(parseList('')).toEqual([]);
  });

  it('keeps only positive integer ids', () => {
    expect(parseIds('3, x, -1, 4.5, 7')).toEqual([3, 7]);
  });

  it('toggles ids without duplicates', () => {
    expect(toggleId([1, 2], 2, true)).toEqual([1, 2]);
    expect(toggleId([1, 2], 3, true)).toEqual([1, 2, 3]);
    expect(toggleId([1, 2], 1, false)).toEqual([2]);
  });

  it('moves rules and ignores out-of-range moves', () => {
    expect(moveItem(['a', 'b', 'c'], 2, 0)).toEqual(['c', 'a', 'b']);
    expect(moveItem(['a', 'b'], 0, -1)).toEqual(['a', 'b']);
  });

  it('flags a rule without criteria as catch-all', () => {
    expect(isCatchAll(emptyRule())).toBe(true);
    expect(isCatchAll({ ...emptyRule(), visitor: 'logged_in' })).toBe(false);
    expect(isCatchAll({ ...emptyRule(), keywords: ['退貨'] })).toBe(false);
  });

  it('validates rule name and agent before save', () => {
    const cfg = baseConfig();
    expect(validateConfig(cfg)).toBeNull();
    cfg.audience_rules = [{ ...emptyRule(), agent: 'kiki' }];
    expect(validateConfig(cfg)).toBe('rule:1:name');
    cfg.audience_rules = [{ ...emptyRule(), name: 'VIP' }];
    expect(validateConfig(cfg)).toBe('rule:1:agent');
    cfg.audience_rules = [{ ...emptyRule(), name: 'VIP', agent: 'kiki' }];
    expect(validateConfig(cfg)).toBeNull();
  });
});
