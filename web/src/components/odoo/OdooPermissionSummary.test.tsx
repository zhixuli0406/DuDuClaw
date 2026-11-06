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
  ])('shows button actions separately from other methods in $locale', ({ locale, messages }) => {
    render(<IntlProvider locale={locale} messages={messages}>
      <OdooPermissionSummary actions={['action', 'action:sale.order', 'execute:sale.order']} />
    </IntlProvider>);
    expect(screen.getByText(`${messages['odoo.permissions.allTables']}：${messages['odoo.permissions.action']}`)).toBeInTheDocument();
    expect(screen.getByText(`sale.order：${messages['odoo.permissions.action']}、${messages['odoo.permissions.execute']}`)).toBeInTheDocument();
    expect(screen.queryByText(new RegExp(messages['odoo.permissions.other']))).not.toBeInTheDocument();
  });
});
