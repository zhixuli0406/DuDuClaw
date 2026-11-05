import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
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
  it('saves Google settings separately from a write-only key, then clears the input', async () => {
    const google = { project_id: 'meeting-project', location: 'asia-southeast1', bucket: 'meeting-audio', language: 'cmn-Hant-TW', model: 'chirp_2', diarization: false };
    const config = { engine: 'google_stt' as const, model: null, max_bytes: 123456, google_stt: google };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    const write = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({ success: true, changes: ['transcription'] });
    const setKey = vi.spyOn(api.system, 'setTranscriptionCredential').mockResolvedValue({ success: true });
    renderWithProviders(<MeetingTranscriptionCard />);
    const keyField = await screen.findByLabelText('Service account JSON key');
    expect(keyField).toHaveAttribute('type', 'password');
    expect(keyField).toHaveValue('');
    expect(screen.getByRole('switch', { name: 'Identify speakers' })).toHaveAttribute('aria-disabled', 'true');
    expect(screen.getByRole('combobox', { name: 'Recognition language' })).toHaveTextContent('cmn-Hant-TW');
    const key = JSON.stringify({ type: 'service_account', private_key: 'test-only' });
    fireEvent.change(keyField, { target: { value: key } });
    fireEvent.change(screen.getByLabelText('Cloud Storage bucket name'), { target: { value: 'new-meeting-audio' } });
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(write).toHaveBeenCalledWith({
      transcription: { ...config, google_stt: { ...google, bucket: 'new-meeting-audio' } },
    });
    expect(setKey).toHaveBeenCalledExactlyOnceWith(key);
    expect(JSON.stringify(write.mock.calls)).not.toContain(key);
    await waitFor(() => expect(keyField).toHaveValue(''));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(write).toHaveBeenLastCalledWith({ transcription: { ...config, google_stt: { ...google, bucket: 'new-meeting-audio' } } });
    expect(setKey).toHaveBeenCalledTimes(1);
  });
  it('keeps the entered key available for retry after a failed save', async () => {
    const config = { engine: 'google_stt' as const, model: null, max_bytes: 123456 };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    vi.spyOn(api.system, 'updateConfig').mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<MeetingTranscriptionCard />);
    const keyField = await screen.findByLabelText('Service account JSON key');
    fireEvent.change(keyField, { target: { value: 'test-key' } });
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Could not load or save transcription settings');
    expect(keyField).toHaveValue('test-key');
  });

  it('keeps the key for retry when the dedicated credential RPC fails', async () => {
    const config = { engine: 'google_stt' as const, model: null, max_bytes: 123456 };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    const write = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({ success: true, changes: ['transcription'] });
    const setKey = vi.spyOn(api.system, 'setTranscriptionCredential').mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<MeetingTranscriptionCard />);
    const keyField = await screen.findByLabelText('Service account JSON key');
    fireEvent.change(keyField, { target: { value: 'test-key' } });
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText('Could not load or save transcription settings');
    expect(setKey).toHaveBeenCalledWith('test-key');
    expect(keyField).toHaveValue('test-key');
    expect(JSON.stringify(write.mock.calls)).not.toContain('test-key');
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });

  it('removes the saved credential through its dedicated RPC only', async () => {
    const config = { engine: 'google_stt' as const, model: null, max_bytes: 123456 };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    const write = vi.spyOn(api.system, 'updateConfig');
    const clearKey = vi.spyOn(api.system, 'clearTranscriptionCredential').mockResolvedValue({ success: true });
    renderWithProviders(<MeetingTranscriptionCard />);
    const keyField = await screen.findByLabelText('Service account JSON key');
    fireEvent.change(keyField, { target: { value: 'unsaved-key' } });
    await userEvent.click(screen.getByRole('button', { name: 'Remove saved key' }));
    await screen.findByText('Saved key removed');
    expect(clearKey).toHaveBeenCalledExactlyOnceWith();
    expect(write).not.toHaveBeenCalled();
    expect(keyField).toHaveValue('');
  });

  it('reports a failed credential removal without claiming it was cleared', async () => {
    const config = { engine: 'google_stt' as const, model: null, max_bytes: 123456 };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    vi.spyOn(api.system, 'clearTranscriptionCredential').mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<MeetingTranscriptionCard />);
    const clearButton = await screen.findByRole('button', { name: 'Remove saved key' });
    await userEvent.click(clearButton);
    await screen.findByText('Could not load or save transcription settings');
    expect(screen.queryByText('Saved key removed')).not.toBeInTheDocument();
    expect(clearButton).toBeEnabled();
  });

  it('offers Google STT with Taiwan language defaults when switching providers', async () => {
    const config = { engine: 'whisper' as const, model: null, max_bytes: 123456 };
    vi.spyOn(api.system, 'config').mockResolvedValue({ config: '', transcription: config });
    const write = vi.spyOn(api.system, 'updateConfig').mockResolvedValue({ success: true, changes: ['transcription'] });
    renderWithProviders(<MeetingTranscriptionCard />);
    await userEvent.click(await screen.findByRole('combobox', { name: 'Transcription engine' }));
    await userEvent.click(await screen.findByRole('option', { name: 'Google Cloud Speech-to-Text' }));
    expect(screen.getByRole('combobox', { name: 'Recognition language' })).toHaveTextContent('cmn-Hant-TW');
    expect(screen.getByRole('combobox', { name: 'Recognition region' })).toHaveTextContent('asia-southeast1');
    fireEvent.change(screen.getByLabelText('Google Cloud project ID'), { target: { value: 'meeting-project' } });
    fireEvent.change(screen.getByLabelText('Cloud Storage bucket name'), { target: { value: 'meeting-audio' } });
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(write).toHaveBeenCalledWith({ transcription: {
      ...config, engine: 'google_stt', google_stt: {
        project_id: 'meeting-project', location: 'asia-southeast1', bucket: 'meeting-audio',
        language: 'cmn-Hant-TW', model: 'chirp_2', diarization: false,
      },
    } });
  });

});
