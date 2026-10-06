import { describe, expect, it } from 'vitest';
import { createIntl } from 'react-intl';
import en from '@/i18n/en.json';
import { errorText } from './workflow-text';

const intl = createIntl({ locale: 'en', messages: en as Record<string, string> });

describe('errorText (F3 closed error codes)', () => {
  it('maps the server code, not the text', () => {
    expect(errorText(intl, { code: 'snapshot_not_latest', message: 'A newer review snapshot exists.' })).toBe(en['workflow.error.snapshotNotLatest']);
    expect(errorText(intl, { code: 'permission_denied', message: 'whatever' })).toBe(en['workflow.error.permission']);
    // An unknown code never shows the raw message.
    const unknown = errorText(intl, { code: 'brand_new_code', message: 'open /home/u/x.db failed' });
    expect(unknown).toBe(en['workflow.error.generic']);
  });
  it('keeps the string fallback for errors without a code', () => {
    expect(errorText(intl, 'fixture evidence expired')).toBe(en['workflow.error.fixtureExpired']);
    expect(errorText(intl, new Error('disk I/O error'))).toBe(en['workflow.error.generic']);
  });
});
