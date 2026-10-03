import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { InferencePage, llamafileFromConfig, llamafileToUpdate } from './InferencePage';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('InferencePage v1.68', () => {
  it('edits typed llamafile fields, drops removed keys and reports the engine reload', async () => {
    const calls: Array<[string, unknown]> = [];
    mockWsClient.call.mockImplementation(async (method: string, params?: unknown) => {
      calls.push([method, params]);
      if (method === 'inference.update') return { success: true, changes: [], engine_reset: true };
      return {
        enabled: true,
        backend: 'openai_compat',
        max_memory_mb: 8192,
        generation: { max_tokens: 512, gpu_layers: -1, context_size: 4096 },
        router: { enabled: false },
        openai_compat: { base_url: '', model: '', api_key_set: false },
        llamafile: {},
        embedding: { model: 'x' },
      };
    });

    renderWithProviders(<InferencePage />);
    await screen.findByText('llamafile local server');
    fireEvent.click(screen.getByRole('switch', { name: 'Enable llamafile' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(calls.some(([m]) => m === 'inference.update')).toBe(true));
    const update = calls.find(([m]) => m === 'inference.update')?.[1] as Record<string, unknown>;
    expect(update.llamafile).toEqual({ enabled: true });
    expect(update).not.toHaveProperty('embedding');
    expect(update).not.toHaveProperty('max_memory_mb');
    expect(update.generation).toEqual({ max_tokens: 512 });
    expect(await screen.findByTestId('engine-reset')).toHaveTextContent('The inference engine has reloaded');
  });

  it('maps llamafile config both ways, ignoring wrong types', () => {
    expect(llamafileToUpdate({ host: '', extra_args: [] }, { host: '127.0.0.1', extra_args: ['-t'] })).toEqual({ host: null, extra_args: null });
    const l = llamafileFromConfig({ enabled: 'yes', port: 8081, extra_args: ['--threads', 8], dir: '' });
    expect(l).toMatchObject({ enabled: undefined, port: 8081, extra_args: ['--threads'] });
    expect(llamafileToUpdate(l)).toEqual({ port: 8081, extra_args: ['--threads'] });
  });
});
