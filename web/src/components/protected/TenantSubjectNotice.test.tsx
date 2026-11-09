import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import { TenantSubjectNotice, tenantSubjectTone } from './TenantSubjectNotice';
import type { GoogleTenantSubjectNotice } from '@/lib/api';

type State = GoogleTenantSubjectNotice['state'];

const CASES: Array<[State, string, 'info' | 'warning' | 'error']> = [
  ['service_mailbox', '使用公司共用信箱', 'info'],
  ['service_mailbox_conflict', '這個共用信箱同時是某位同仁的登入信箱', 'error'],
  ['attested_person', '管理員已確認是本人', 'info'],
  ['attestation_required', '需要管理員確認這個信箱屬於哪位同仁', 'warning'],
  ['ambiguous', '有多個帳號使用這個信箱，無法判斷', 'warning'],
  ['person_inactive', '這位同仁的帳號已停用', 'warning'],
  ['not_admissible', '這個信箱不能用來代表同仁', 'error'],
  ['unavailable', '暫時無法確認，請稍後再試', 'warning'],
  ['users_unavailable', '暫時無法確認，請稍後再試', 'warning'],
  ['refused', '已拒絕使用這個信箱', 'error'],
];

describe('TenantSubjectNotice', () => {
  it.each(CASES)('renders %s', (state, text, tone) => {
    render(
      <IntlProvider locale="zh-TW" messages={zhTW as Record<string, string>}>
        <TenantSubjectNotice notice={{ state }} />
      </IntlProvider>,
    );
    const el = screen.getByTestId('tenant-subject-notice');
    expect(el.textContent).toContain(text);
    expect(el.getAttribute('data-tone')).toBe(tone);
    expect(tenantSubjectTone(state)).toBe(tone);
  });
});
