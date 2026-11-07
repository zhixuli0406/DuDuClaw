import { describe, it, expect, vi } from 'vitest';
import { createIntl } from 'react-intl';
import en from './en.json';
import zhTW from './zh-TW.json';
import jaJP from './ja-JP.json';

/**
 * v1.68 (W2): every message id exists in all three catalogues. ja-JP falls
 * back to English at runtime (i18n/index.ts), which hides a missing Japanese
 * string instead of failing — this keeps the three files in step.
 */
function missing(from: Record<string, unknown>, into: Record<string, unknown>): string[] {
  return Object.keys(from).filter((k) => !(k in into));
}

describe('i18n catalogue parity', () => {
  it('zh-TW, en and ja-JP carry the same message ids', () => {
    expect(missing(zhTW, en)).toEqual([]);
    expect(missing(en, zhTW)).toEqual([]);
    expect(missing(zhTW, jaJP)).toEqual([]);
    expect(missing(jaJP, zhTW)).toEqual([]);
  });

  it('no message is empty', () => {
    for (const [name, cat] of [['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const) {
      const empty = Object.entries(cat as Record<string, unknown>)
        .filter(([, v]) => typeof v !== 'string' || v.trim() === '')
        .map(([k]) => `${name}:${k}`);
      expect(empty).toEqual([]);
    }
  });
});

describe('MCP redirect address help', () => {
  it.each([
    ['en', en, '<dashboard>/oauth/mcp/callback'],
    ['zh-TW', zhTW, '<儀表板網址>/oauth/mcp/callback'],
    ['ja-JP', jaJP, '<ダッシュボード>/oauth/mcp/callback'],
  ] as const)('formats the literal redirect address in %s without ICU errors', (locale, messages, address) => {
    const onError = vi.fn();
    const intl = createIntl({ locale, messages, onError });
    expect(intl.formatMessage({ id: 'mcp.remote.ownClientHelp' })).toContain(address);
    expect(onError).not.toHaveBeenCalled();
  });
});


describe('literal command and HTML help', () => {
  it.each([['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const)(
    'formats literal angle brackets in %s without ICU errors', (locale, messages) => {
      const onError = vi.fn();
      const intl = createIntl({ locale, messages, onError });
      for (const [id, literal] of [
        ['channels.detail.access.requirePairing.help', '/pair <'],
        ['distributor.issue.result.guide', 'duduclaw license activate <blob>'],
        ['widgets.compose.htmlHint', '<html>/<head>/<body>'],
        ['secaudit.list.empty.desc', 'duduclaw secaudit <repo> --save'],
      ]) {
        expect(intl.formatMessage({ id })).toContain(literal);
      }
      expect(onError).not.toHaveBeenCalled();
    },
  );
});
