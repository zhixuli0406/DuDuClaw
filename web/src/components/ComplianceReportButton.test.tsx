import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { ComplianceReportButton } from './ComplianceReportButton';

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { role: 'admin' } as never });
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
});
describe('ComplianceReportButton', () => {
  it.each(['manager', 'employee'])('hides reports from %s', (role) => {
    useAuthStore.setState({ user: { role } as never });
    renderWithProviders(<ComplianceReportButton />);
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
  it.each(['personal', undefined])('hides reports without confirmed Enterprise edition: %s', (edition_profile) => {
    useSystemStore.setState({ status: { edition_profile } as never });
    renderWithProviders(<ComplianceReportButton />);
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
  it('defaults to masked output and requires explicit selection for full values', async () => {
    mockWsClient.call.mockRejectedValue(new Error('offline'));
    const user = userEvent.setup();
    renderWithProviders(<ComplianceReportButton />);
    await user.click(screen.getByRole('button', { name: 'Compliance report' }));
    const checkbox = screen.getByRole('checkbox');
    expect(checkbox).not.toBeChecked();
    fireEvent.change(screen.getByLabelText('Start date'), { target: { value: '2026-11-01' } });
    fireEvent.change(screen.getByLabelText('End date (inclusive)'), { target: { value: '2026-11-02' } });
    await user.click(screen.getByRole('button', { name: 'Generate and download' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('compliance.generate', expect.objectContaining({ include_personal_data: false, format: 'xlsx', language: 'en' })));
    await user.click(checkbox);
    await user.click(screen.getByRole('button', { name: 'Generate and download' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenLastCalledWith('compliance.generate', expect.objectContaining({ include_personal_data: true })));
  });
});
