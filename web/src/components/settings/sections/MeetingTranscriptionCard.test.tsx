import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { MeetingTranscriptionCard } from './MeetingTranscriptionCard';

beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({ user: { role: 'admin' } as never });
});

describe('meeting transcription settings', () => {
  it('does not request or display administrator settings for employees', () => {
    useAuthStore.setState({ user: { role: 'employee' } as never });
    const read = vi.spyOn(api.system, 'config');
    const { container } = renderWithProviders(<MeetingTranscriptionCard />);
    expect(container).toBeEmptyDOMElement();
    expect(read).not.toHaveBeenCalled();
  });
  it('keeps saving disabled when configuration could not be read', async () => {
    vi.spyOn(api.system, 'config').mockRejectedValue(new Error('unavailable'));
    const write = vi.spyOn(api.system, 'updateConfig');
    renderWithProviders(<MeetingTranscriptionCard />);
    await screen.findByText('Could not load or save transcription settings');
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(write).not.toHaveBeenCalled();
  });
  it('saves the selected provider without dropping the configured size limit', async () => {
    const config = { engine: 'whisper' as const, model: null, max_bytes: 123456 };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    const write = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({ success: true, changes: ['transcription'] });
    renderWithProviders(<MeetingTranscriptionCard />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled());
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(write).toHaveBeenCalledWith({ transcription: config });
  });
});
