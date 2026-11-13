import { describe, expect, it } from 'vitest';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import {
  AUTOPILOT_HISTORY_RESULTS,
  describeHistoryResult,
  historyResultMessageId,
} from './autopilot-history';

describe('autopilot history statuses', () => {
  it('covers every status the gateway writes', () => {
    for (const status of [
      'success', 'failure', 'skipped', 'awaiting_approval', 'blocked_no_approval_service',
      'approval_error', 'denied', 'expired', 'stale_after_approval', 'execution_uncertain',
      'blocked_breaker_open', 'circuit_open', 'circuit_half_open', 'circuit_closed',
      'screen_drop', 'screen_unavailable', 'paused_by_human',
    ]) {
      expect(AUTOPILOT_HISTORY_RESULTS).toContain(status);
    }
  });

  it('has a label for every status in all three languages', () => {
    for (const [name, messages] of [['en', en], ['zh-TW', zhTW], ['ja-JP', jaJP]] as const) {
      for (const status of AUTOPILOT_HISTORY_RESULTS) {
        const id = historyResultMessageId(status);
        expect(Object.hasOwn(messages, id), `${name}: ${id}`).toBe(true);
        expect((messages as Record<string, string>)[id], `${name}: ${id}`).not.toBe('');
      }
    }
  });

  it('gives each status a tone', () => {
    expect(describeHistoryResult('success').tone).toBe('success');
    expect(describeHistoryResult('awaiting_approval').tone).toBe('warning');
    expect(describeHistoryResult('expired').tone).toBe('warning');
    expect(describeHistoryResult('denied').tone).toBe('neutral');
    expect(describeHistoryResult('skipped').tone).toBe('neutral');
    for (const s of ['failure', 'approval_error', 'blocked_no_approval_service', 'blocked_breaker_open', 'execution_uncertain']) {
      expect(describeHistoryResult(s).tone, s).toBe('error');
    }
  });

  it('renders an unknown status raw and neutral, never crashing', () => {
    expect(describeHistoryResult('some_future_status')).toEqual({ messageId: null, raw: 'some_future_status', tone: 'neutral' });
    // `empty` is a sibling message id (`autopilot.history.empty`), not a status.
    expect(describeHistoryResult('empty').messageId).toBeNull();
    expect(describeHistoryResult(undefined).messageId).toBeNull();
    expect(describeHistoryResult(42).raw).toBe('42');
  });
});
