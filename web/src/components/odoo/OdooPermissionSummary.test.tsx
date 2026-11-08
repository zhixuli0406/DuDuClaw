import { describe, expect, it } from 'vitest';
import { screen } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { render } from '@/test/render';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import { OdooPermissionSummary } from './OdooPermissionSummary';

describe('Odoo permission summary', () => {
  it.each([
    { locale: 'en', messages: en },
    { locale: 'zh-TW', messages: zhTW },
    { locale: 'ja-JP', messages: jaJP },
  ])('shows effective blocks and inheritance with unrestricted actions in $locale', ({ locale, messages }) => {
    const { rerender } = render(<IntlProvider locale={locale} messages={messages}>
      <OdooPermissionSummary actions={[]} effectiveBlockedModels={['hr.employee']} unblockInherited />
    </IntlProvider>);
    expect(screen.getByText(messages['odoo.permissions.effectiveBlockedModels'])).toBeInTheDocument();
    expect(screen.getByText(messages['odoo.permissions.unblockInherited'])).toBeInTheDocument();
    expect(screen.getByText('hr.employee')).toBeInTheDocument();
    rerender(<IntlProvider locale={locale} messages={messages}>
      <OdooPermissionSummary actions={[]} effectiveBlockedModels={[]} unblockInherited={false} />
    </IntlProvider>);
    expect(screen.queryByText(messages['odoo.permissions.unblockInherited'])).not.toBeInTheDocument();
    expect(screen.getByText(messages['odoo.permissions.noBlockedModels'])).toBeInTheDocument();
  });
  it.each([
    { locale: 'en', messages: en },
    { locale: 'zh-TW', messages: zhTW },
    { locale: 'ja-JP', messages: jaJP },
  ])('shows button actions separately from other methods in $locale', ({ locale, messages }) => {
    render(<IntlProvider locale={locale} messages={messages}>
      <OdooPermissionSummary actions={['action', 'action:sale.order', 'execute:sale.order']} />
    </IntlProvider>);
    expect(screen.getByText(`${messages['odoo.permissions.allTables']}：${messages['odoo.permissions.action']}`)).toBeInTheDocument();
    expect(screen.getByText(`sale.order：${messages['odoo.permissions.action']}、${messages['odoo.permissions.execute']}`)).toBeInTheDocument();
    expect(screen.queryByText(new RegExp(messages['odoo.permissions.other']))).not.toBeInTheDocument();
  });
  it.each([
    { locale: 'en', messages: en },
    { locale: 'zh-TW', messages: zhTW },
    { locale: 'ja-JP', messages: jaJP },
  ])('names the per-table general create / update opt-in in $locale', ({ locale, messages }) => {
    render(<IntlProvider locale={locale} messages={messages}>
      <OdooPermissionSummary actions={['write:crm.lead', 'generic_write:crm.lead', 'generic_write']} />
    </IntlProvider>);
    expect(screen.getByText(`crm.lead：${messages['odoo.permissions.write']}、${messages['odoo.permissions.genericWrite']}`)).toBeInTheDocument();
    // A bare token is not an all-tables opt-in; it stays visible as an unknown entry.
    expect(screen.getByText(`${messages['odoo.permissions.other']}：generic_write`)).toBeInTheDocument();
  });
  it('uses the owner-approved zh-TW label', () => {
    expect(zhTW['odoo.permissions.genericWrite']).toBe('允許通用新增／修改');
  });
});
