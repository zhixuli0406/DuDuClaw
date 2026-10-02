import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { InferencePage } from './InferencePage';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('InferencePage', () => {
  it('renders the slim header and Save action', async () => {
    mockWsClient.call.mockResolvedValue({
      enabled: true,
      backend: 'llama_cpp',
      generation: { max_tokens: 512 },
      router: { enabled: false },
      openai_compat: { base_url: '', model: '', api_key_set: false },
    });

    renderWithProviders(<InferencePage />);

    // Header title (nav.inference) + primary Save button render immediately.
    expect(screen.getByRole('heading', { name: 'Inference' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save' })).toBeInTheDocument();

    // After the async load resolves, a config section is visible.
    expect(await screen.findByText('Generation')).toBeInTheDocument();
  });

  it('labels a removed backend and no longer offers controls nothing reads', async () => {
    mockWsClient.call.mockResolvedValue({
      enabled: true,
      backend: 'llama_cpp',
      max_memory_mb: 8192,
      generation: { max_tokens: 512, gpu_layers: -1, context_size: 4096 },
      router: { enabled: false },
      openai_compat: { base_url: '', model: '', api_key_set: false },
    });

    renderWithProviders(<InferencePage />);

    expect(
      await screen.findByText('llama_cpp (no longer supported, choose OpenAI-compatible server)'),
    ).toBeInTheDocument();
    expect(screen.queryByText(/GPU Layers/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Context Size/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Max Memory/)).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText('llama_cpp')).not.toBeInTheDocument();
  });

  it('always sends the backend so "not set" ("") clears a stored value, and shows a refusal', async () => {
    const calls: Array<[string, unknown]> = [];
    mockWsClient.call.mockImplementation(async (method: string, params?: unknown) => {
      calls.push([method, params]);
      if (method === 'inference.update') {
        throw new Error("inference.backend 'llama_cpp' is not supported: use \"openai_compat\"");
      }
      return {
        enabled: true,
        generation: {},
        router: {},
        openai_compat: { base_url: '', model: '', api_key_set: false },
      };
    });

    renderWithProviders(<InferencePage />);
    await screen.findByText('Generation');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('The settings were not saved');
    expect(alert.textContent).toContain('is not supported');
    const update = calls.find(([m]) => m === 'inference.update');
    expect((update?.[1] as { backend?: string }).backend).toBe('');
  });
});

