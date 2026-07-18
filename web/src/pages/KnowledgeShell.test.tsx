import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { KnowledgeShell } from './KnowledgeShell';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('KnowledgeShell', () => {
  it('renders the collection header and personal/shared tabs', () => {
    renderWithProviders(<KnowledgeShell />);
    expect(screen.getByRole('heading', { name: 'Knowledge Hub' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Personal' })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: 'Shared' })).toBeInTheDocument();
  });

  it('switches to the shared tab and reveals the shared-only segment', async () => {
    const user = userEvent.setup();
    renderWithProviders(<KnowledgeShell />);
    // Personal is the default panel; the shared-only "Namespace Policy" segment
    // is not mounted yet.
    expect(screen.queryByRole('radio', { name: 'Namespace Policy' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: 'Shared' }));
    expect(await screen.findByRole('radio', { name: 'Namespace Policy' })).toBeInTheDocument();
  });
});
