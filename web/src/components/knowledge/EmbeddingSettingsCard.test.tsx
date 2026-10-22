import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import type { KbEmbeddingSettings } from '@/lib/knowledge-embedding-api';
import { EmbeddingSettingsCard } from './EmbeddingSettingsCard';
import { disclosureTarget, formatBytes, percent, shouldPoll } from './embedding-settings';

function status(over: Partial<KbEmbeddingSettings> = {}): KbEmbeddingSettings {
  return {
    provider: 'local',
    available: false,
    mode: 'keyword_only',
    reason: '尚未安裝向量模型',
    coverage: 0,
    model_id: null,
    egress: 'local',
    chunks_with_vectors: 0,
    chunks_total: 12,
    vector_rows: 0,
    max_vector_rows: 200000,
    local_model: 'multilingual-e5-small',
    local_loaded: false,
    platform_supported: true,
    models: [
      {
        id: 'multilingual-e5-small',
        display_name: 'multilingual-e5-small (int8)',
        license: 'MIT',
        license_source: '',
        repo: 'intfloat/multilingual-e5-small',
        revision: 'x',
        dim: 384,
        speed: 'fast',
        size_bytes: 167_000_000,
        model_bytes: 135_000_000,
        installed: false,
        problem: '尚未安裝向量模型',
        selected: true,
      },
    ],
    job: { model: null, state: 'idle', progress: null, error: null },
    cloud: null,
    cloud_providers: [
      { id: 'openai', vendor: 'OpenAI', default_model: 'text-embedding-3-small', host: 'api.openai.com' },
      { id: 'gemini', vendor: 'Google Gemini', default_model: 'gemini-embedding-001', host: 'generativelanguage.googleapis.com' },
    ],
    acknowledgement: null,
    redaction: { ready: true },
    is_admin: true,
    ...over,
  };
}

let current = status();

beforeEach(() => {
  vi.clearAllMocks();
  current = status();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'knowledge.embedding.status') return Promise.resolve(current);
    if (method === 'knowledge.embedding.select') return Promise.resolve({ saved: true, queued: 0 });
    if (method === 'knowledge.embedding.install') return Promise.resolve({ started: true, installed: false });
    return Promise.resolve(null);
  });
});

describe('EmbeddingSettingsCard', () => {
  it('says plainly why search is keyword-only and offers the install', async () => {
    const user = userEvent.setup();
    renderWithProviders(<EmbeddingSettingsCard />);
    expect(await screen.findByText('Keyword matching only')).toBeInTheDocument();
    expect(screen.getByText('尚未安裝向量模型', { selector: 'p' })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Install \(/ }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('knowledge.embedding.install', { model: 'multilingual-e5-small' }),
    );
  });

  it('shows coverage when vectors are on', async () => {
    current = status({ available: true, mode: 'hybrid', reason: null, coverage: 0.5, chunks_with_vectors: 6, chunks_total: 12 });
    renderWithProviders(<EmbeddingSettingsCard />);
    expect(await screen.findByText('Meaning + keyword search')).toBeInTheDocument();
    expect(screen.getByText('50% done (6 of 12 passages)')).toBeInTheDocument();
    expect(screen.getByText(/Computed on this computer/)).toBeInTheDocument();
  });

  it('a cloud provider needs the disclosure tick before it is saved', async () => {
    const user = userEvent.setup();
    renderWithProviders(<EmbeddingSettingsCard />);
    await screen.findByText('Keyword matching only');
    await user.click(screen.getByRole('radio', { name: 'OpenAI' }));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText(/questions will be sent to OpenAI \(api\.openai\.com\), model text-embedding-3-small/)).toBeInTheDocument();
    const confirm = screen.getByRole('button', { name: 'Use OpenAI' });
    expect(confirm).toBeDisabled();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('knowledge.embedding.select', expect.anything());
    await user.click(screen.getByRole('checkbox'));
    await user.click(confirm);
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith(
        'knowledge.embedding.select',
        expect.objectContaining({ provider: 'openai', acknowledge_disclosure: true }),
      ),
    );
  });

  it('warns in the dialog when redaction is off', async () => {
    const user = userEvent.setup();
    current = status({ redaction: { ready: false, reason: '去識別化（[redaction]）尚未啟用' } });
    renderWithProviders(<EmbeddingSettingsCard />);
    await screen.findByText('Keyword matching only');
    await user.click(screen.getByRole('radio', { name: 'Google Gemini' }));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/Redaction is not switched on/);
  });

  it('non-admins see the status but no controls', async () => {
    current = status({ is_admin: false });
    renderWithProviders(<EmbeddingSettingsCard />);
    await screen.findByText('Keyword matching only');
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Install/ })).not.toBeInTheDocument();
  });
});

describe('embedding-settings helpers', () => {
  it('formats and polls', () => {
    expect(formatBytes(118_346_824)).toBe('113 MB');
    expect(formatBytes(0)).toBe('0 MB');
    expect(formatBytes(2 * 1024 ** 3)).toBe('2.0 GB');
    expect(percent(0.333)).toBe(33);
    expect(percent(2)).toBe(100);
    expect(shouldPoll(null)).toBe(false);
    expect(shouldPoll(status({ job: { model: 'x', state: 'downloading', progress: null, error: null } }))).toBe(true);
    expect(shouldPoll(status({ available: true, chunks_with_vectors: 3 }))).toBe(true);
    expect(shouldPoll(status({ available: true, chunks_with_vectors: 12 }))).toBe(false);
  });

  it('names the configured host for the configured provider', () => {
    const s = status({ cloud: { provider: 'openai', vendor: 'OpenAI', host: 'proxy.example.com', model: 'm' } });
    expect(disclosureTarget(s, 'openai', '')).toEqual({ vendor: 'OpenAI', host: 'proxy.example.com', model: 'text-embedding-3-small' });
    expect(disclosureTarget(s, 'gemini', 'g-1').host).toBe('generativelanguage.googleapis.com');
  });
});
