import { describe, expect, it } from 'vitest';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';
import { ADVANCED_SETTINGS, LEGACY_ADVANCED_TABS, filterAdvancedSettings } from './advanced-settings-catalog';

describe('advanced settings search', () => {
  it.each([['en', en], ['zh-TW', zh], ['ja-JP', ja]] as const)('searches translated names and descriptions in %s', (_locale, messages) => {
    expect(filterAdvancedSettings(messages['settings.knowledgeSearch'], messages).map((entry) => entry.id)).toContain('knowledgeSearch');
    expect(filterAdvancedSettings(messages['settings.knowledgeSearch.desc'], messages).map((entry) => entry.id)).toContain('knowledgeSearch');
    for (const entry of ADVANCED_SETTINGS) {
      expect(Object.hasOwn(messages, entry.title)).toBe(true);
      expect(Object.hasOwn(messages, entry.description)).toBe(true);
    }
  });

  it('searches individual field help, normalizes case and whitespace, and requires all terms', () => {
    expect(filterAdvancedSettings('  GLOBAL    BACKGROUND  ', en)).toEqual(filterAdvancedSettings('global background', en));
    expect(filterAdvancedSettings('global background', en).map((entry) => entry.id)).toContain('utility');
    expect(filterAdvancedSettings('Memory trust guard', en).map((entry) => entry.id)).toEqual(['security']);
    expect(filterAdvancedSettings('background impossible-setting-name', en)).toEqual([]);
    expect(filterAdvancedSettings('  ', en)).toEqual(ADVANCED_SETTINGS);
  });

  it('gives every former tab a unique stable anchor', () => {
    const anchors = ADVANCED_SETTINGS.map((entry) => entry.id);
    expect(new Set(anchors).size).toBe(anchors.length);
    for (const tab of LEGACY_ADVANCED_TABS) expect(anchors).toContain(tab);
  });
});
