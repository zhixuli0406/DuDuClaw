import { setStandardProductPolicyFixture } from '@/test/product-policy';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const configMock = vi.fn();
const updateMock = vi.fn();
const statusMock = vi.fn();
const installMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    voice: { kokoroStatus: () => statusMock(), kokoroInstall: () => installMock() },
    system: {
      config: () => configMock(),
      updateConfig: (p: unknown) => updateMock(p),
    },
  },
}));

import { VoiceTab } from './VoiceTab';

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, json: async () => null }));
  configMock.mockResolvedValue({
    voice: {
      asr_provider: 'whisper-api',
      tts_provider: 'kokoro',
      asr_language: 'ja',
      tts_voice: 'nova',
      voice_reply_enabled: true,
    },
  });
  updateMock.mockResolvedValue({ success: true });
  statusMock.mockResolvedValue({ installed: false, size_bytes: 134000000, missing_files: ['model'], ort_ok: false, state: 'absent', progress: null, error: null });
  installMock.mockResolvedValue({ started: true });
});

describe('<VoiceTab>', () => {
  it('saves only the voice keys the gateway reads, leaving the others untouched', async () => {
    renderWithProviders(<VoiceTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    expect(await screen.findByText('Text-to-Speech')).toBeTruthy();

    fireEvent.click(screen.getAllByRole('button', { name: 'Save' })[0]);
    await waitFor(() => expect(updateMock).toHaveBeenCalledTimes(1));
    expect(updateMock.mock.calls[0][0]).toEqual({
      voice: { tts_provider: 'kokoro', tts_voice: 'nova', tts_command: '' },
    });
  });

  it('normalizes deprecated settings to automatic routing', async () => {
    configMock.mockResolvedValue({ voice: { tts_provider: 'edge-tts', tts_voice: '' } });
    renderWithProviders(<VoiceTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    await screen.findByRole('button', { name: 'Download model' });
    fireEvent.click(screen.getAllByRole('button', { name: 'Save' })[0]);
    await waitFor(() => expect(updateMock).toHaveBeenCalledWith({ voice: { tts_provider: 'auto', tts_voice: '', tts_command: '' } }));
  });

  it('does not show a download card when speech is off', async () => {
    configMock.mockResolvedValue({ voice: { tts_provider: 'off', tts_voice: '' } });
    renderWithProviders(<VoiceTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Download model' })).toBeNull());
  });

  it('downloads the local model and refreshes its status', async () => {
    renderWithProviders(<VoiceTab />);
    const download = await screen.findByRole('button', { name: 'Download model' });
    await waitFor(() => expect((download as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(download);
    await waitFor(() => expect(installMock).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(statusMock).toHaveBeenCalledTimes(2));
    expect(screen.getByText(/134 MB/)).toBeTruthy();
    expect(screen.getByText(/mainland accent/)).toBeTruthy();
  });

  it('offers verification and reinstall for an installed model', async () => {
    statusMock.mockResolvedValue({ installed: true, size_bytes: 134000000, missing_files: [], ort_ok: true, state: 'ready', progress: null, error: null });
    renderWithProviders(<VoiceTab />);
    const reinstall = await screen.findByRole('button', { name: 'Verify / reinstall' });
    expect((reinstall as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(reinstall);
    await waitFor(() => expect(installMock).toHaveBeenCalledTimes(1));
  });

  it('surfaces status failures', async () => {
    statusMock.mockRejectedValue(new Error('denied'));
    renderWithProviders(<VoiceTab />);
    expect(await screen.findByText('Could not load or install the voice model')).toBeTruthy();
  });

  it('no longer shows the voice-reply, recognition or language controls', async () => {
    renderWithProviders(<VoiceTab />);
    await screen.findByText('Text-to-Speech');
    expect(screen.queryByText('Voice Reply Mode')).toBeNull();
    expect(screen.queryByText('Speech Recognition')).toBeNull();
    expect(screen.queryByText('Language')).toBeNull();
  });
});

// This suite exercises the standard installation catalog.
beforeEach(setStandardProductPolicyFixture);
