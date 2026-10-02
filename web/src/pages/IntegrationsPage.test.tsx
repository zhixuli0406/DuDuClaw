import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { useSystemStore } from '@/stores/system-store';
import { useAuthStore } from '@/stores/auth-store';

// The four tab bodies are heavy pages with their own RPCs; this test is about
// which tabs exist, so each body is a marker.
vi.mock('./McpPage', () => ({ McpPage: () => <div>mcp-body</div> }));
vi.mock('./McpKeysPage', () => ({ McpKeysPage: () => null }));
vi.mock('./OdooPage', () => ({ OdooPage: () => <div>odoo-body</div> }));
vi.mock('./IdentityPage', () => ({ IdentityPage: () => <div>identity-body</div> }));
vi.mock('./GoogleIntegrationPage', () => ({ GoogleIntegrationPage: () => <div>google-body</div> }));

import { IntegrationsPage } from './IntegrationsPage';

function renderAt(url: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[url]}>
        <IntegrationsPage />
      </MemoryRouter>
    </IntlProvider>,
  );
}

function setEdition(profile: 'personal' | 'enterprise') {
  useSystemStore.setState({ status: { edition_profile: profile } } as never);
}

beforeEach(() => {
  useAuthStore.setState({
    user: { id: 'u1', email: 'u1@test.local', display_name: 'U1', role: 'admin', status: 'active' },
  } as never);
});

describe('<IntegrationsPage> identity tab', () => {
  it('is offered on the enterprise edition', () => {
    setEdition('enterprise');
    renderAt('/manage/integrations?tab=identity');
    expect(screen.getByRole('tab', { name: /Identity/ })).toBeTruthy();
    expect(screen.getByText('identity-body')).toBeTruthy();
  });

  it('is hidden on the personal edition, and a deep link falls back to the first tab', () => {
    setEdition('personal');
    renderAt('/manage/integrations?tab=identity');
    expect(screen.queryByRole('tab', { name: /Identity/ })).toBeNull();
    expect(screen.queryByText('identity-body')).toBeNull();
    expect(screen.getByText('mcp-body')).toBeTruthy();
  });
});
