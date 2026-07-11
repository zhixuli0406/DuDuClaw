import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentStatusGlyph } from './AgentStatusGlyph';

describe('AgentStatusGlyph', () => {
  it('exposes an accessible status label per state', () => {
    renderWithProviders(<AgentStatusGlyph state="replying" showLabel />);
    // role=status with an aria-label; label text also visible when showLabel.
    expect(screen.getByRole('status', { name: 'Replying' })).toBeInTheDocument();
    expect(screen.getByText('Replying')).toBeInTheDocument();
  });

  it('renders the spinner variant for tool_running', () => {
    const { container } = renderWithProviders(<AgentStatusGlyph state="tool_running" />);
    expect(container.querySelector('.animate-spin')).not.toBeNull();
  });

  it('renders an expanding ring for awaiting_approval', () => {
    const { container } = renderWithProviders(
      <AgentStatusGlyph state="awaiting_approval" />,
    );
    expect(container.querySelector('.glyph-approval-ring')).not.toBeNull();
  });

  it('hides the label when showLabel is false', () => {
    renderWithProviders(<AgentStatusGlyph state="paused" />);
    // Still accessible via aria-label, but no visible text node.
    expect(screen.getByRole('status', { name: 'Paused' })).toBeInTheDocument();
    expect(screen.queryByText('Paused')).not.toBeInTheDocument();
  });
});
