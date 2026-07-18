import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
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
});
