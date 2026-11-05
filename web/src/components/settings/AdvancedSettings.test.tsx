import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { useAuthStore } from '@/stores/auth-store';
import { mockWsClient } from '@/test/mocks';
import { AdvancedSettings } from './AdvancedSettings';

vi.mock('./sections/SystemTab', () => ({ SystemTab: ({ section }: { section: string }) => <input aria-label={`draft-${section}`} /> }));

function renderAdvanced(path = '/app/system/settings/advanced') {
  return render(<IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[path]}><AdvancedSettings /></MemoryRouter></IntlProvider>);
}

beforeEach(() => {
  useAuthStore.setState({ user: { id: 'admin', username: 'admin', display_name: 'Admin', role: 'admin' } } as never);
  mockWsClient.call.mockResolvedValue({});
});

describe('AdvancedSettings', () => {
  it('filters visible cards immediately and preserves an open panel draft when searching', () => {
    renderAdvanced();
    fireEvent.click(screen.getByRole('button', { name: en['settings.utility.title'] }));
    fireEvent.change(screen.getByLabelText('draft-models'), { target: { value: 'unsaved model' } });
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'impossible-setting-name' } });
    expect(screen.getByRole('status')).toHaveTextContent(en['advancedSettings.empty']);
    expect(screen.queryByRole('button', { name: en['settings.utility.title'] })).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'background tasks' } });
    expect(screen.getByLabelText('draft-models')).toHaveValue('unsaved model');
    expect(screen.queryByRole('button', { name: en['settings.system'] })).not.toBeInTheDocument();
  });

  it('opens the section addressed by a deep-link anchor', async () => {
    renderAdvanced('/app/system/settings/advanced#utility');
    await waitFor(() => expect(screen.getByRole('button', { name: en['settings.utility.title'] })).toHaveAttribute('aria-expanded', 'true'));
    expect(screen.getByLabelText('draft-models')).toBeInTheDocument();
  });

  it.each(['manager', 'employee'] as const)('does not render or load panels for %s', (role) => {
    useAuthStore.setState({ user: { id: 'user', username: 'user', display_name: 'User', role } } as never);
    mockWsClient.call.mockClear();
    const { container } = renderAdvanced('/app/system/settings/advanced#system');
    expect(container).toBeEmptyDOMElement();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });
});
