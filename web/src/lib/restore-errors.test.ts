import { describe, expect, it } from 'vitest';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import { KNOWN_RESTORE_ERRORS, describeRestoreError, errorCode, needsAdminHelp, serverMessage } from './restore-errors';

describe('restore error mapping', () => {
  it('reads the code from {code, message}, an Error carrying a code, or a bare code string', () => {
    expect(errorCode({ code: 'SECOND_ADMIN_REQUIRED', message: '需要另一位管理員' })).toBe('SECOND_ADMIN_REQUIRED');
    expect(errorCode(new Error('PASSKEY_CANCELLED'))).toBe('PASSKEY_CANCELLED');
    expect(errorCode('NO_SECOND_ADMIN')).toBe('NO_SECOND_ADMIN');
    expect(errorCode('找不到這筆申請。')).toBeNull();
  });

  it('maps a known code to friendly text and keeps the server sentence', () => {
    const d = describeRestoreError({ code: 'SELF_APPROVAL_COOLING', message: '冷靜期尚未結束。' });
    expect(d.friendlyId).toBe('restoreFlow.error.SELF_APPROVAL_COOLING');
    expect(d.detail).toBe('冷靜期尚未結束。');
    expect(d.fallback).toBe('');
  });

  it('shows an unknown code by its server message only', () => {
    const d = describeRestoreError({ code: 'SOMETHING_NEW', message: '伺服器說明。' });
    expect(d.friendlyId).toBeNull();
    expect(d.detail).toBe('伺服器說明。');
  });

  it('never shows a bare code as the message', () => {
    expect(serverMessage('ROOT_UNAVAILABLE')).toBeNull();
    expect(describeRestoreError('ROOT_UNAVAILABLE').friendlyId).toBe('restoreFlow.error.ROOT_UNAVAILABLE');
  });

  it('sends the owner to an Admin for factor / credential changes', () => {
    expect(needsAdminHelp('FACTOR_NOT_PREEXISTING')).toBe(true);
    expect(needsAdminHelp('CREDENTIAL_CHANGED')).toBe(true);
    for (const code of ['FACTOR_NOT_ENROLLED', 'FACTOR_BASELINE_MISSING', 'FACTOR_DENIED']) {
      expect(needsAdminHelp(code)).toBe(true);
    }
    for (const code of ['FACTOR_NOT_STRONGEST', 'FACTOR_RATE_LIMITED', 'FACTOR_FAILED', 'FACTOR_UNAVAILABLE']) {
      expect(needsAdminHelp(code)).toBe(false);
    }
  });

  it('has a friendly sentence for every known code in all three languages', () => {
    for (const code of KNOWN_RESTORE_ERRORS) {
      const id = `restoreFlow.error.${code}`;
      expect(en, id).toHaveProperty([id]);
      expect(zhTW, id).toHaveProperty([id]);
      expect(jaJP, id).toHaveProperty([id]);
    }
  });
});
