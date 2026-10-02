import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const configMock = vi.fn();
const updateMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
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
      tts_provider: 'edge-tts',
      asr_language: 'ja',
      tts_voice: 'nova',
      voice_reply_enabled: true,
    },
  });
  updateMock.mockResolvedValue({ success: true });
});

describe('<VoiceTab>', () => {
  it('saves only the voice keys the gateway reads, leaving the others untouched', async () => {
    renderWithProviders(<VoiceTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    expect(await screen.findByText('Text-to-Speech')).toBeTruthy();

    fireEvent.click(screen.getAllByRole('button', { name: 'Save' })[0]);
    await waitFor(() => expect(updateMock).toHaveBeenCalledTimes(1));
    expect(updateMock.mock.calls[0][0]).toEqual({
      voice: { tts_provider: 'edge-tts', tts_voice: 'nova' },
    });
  });

  it('no longer shows the voice-reply, recognition or language controls', async () => {
    renderWithProviders(<VoiceTab />);
    await screen.findByText('Text-to-Speech');
    expect(screen.queryByText('Voice Reply Mode')).toBeNull();
    expect(screen.queryByText('Speech Recognition')).toBeNull();
    expect(screen.queryByText('Language')).toBeNull();
  });
});
