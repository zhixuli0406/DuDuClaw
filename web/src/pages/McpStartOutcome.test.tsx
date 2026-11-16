import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { McpStartOutcome } from './McpPage';

describe('MCP startup outcome', () => {
  it('shows successful probe tool count', () => {
    renderWithProviders(<McpStartOutcome result={{ ok: true, tool_count: 3 }} />);
    expect(screen.getByText(/Connected/)).toBeInTheDocument();
    expect(screen.getByText(/3 tools/)).toBeInTheDocument();
  });
  it('shows a fixed fallback instead of arbitrary error data', () => {
    renderWithProviders(<McpStartOutcome result={{ ok: false, reason_code: 'secret-from-upstream' }} />);
    expect(screen.getByText(/Server failed to connect/)).toBeInTheDocument();
    expect(screen.queryByText(/secret-from-upstream/)).not.toBeInTheDocument();
  });
  it('shows a translated probe timeout', () => {
    renderWithProviders(<McpStartOutcome result={{ ok: false, reason_code: 'timeout' }} />);
    expect(screen.getByText(/Start probe timed out/)).toBeInTheDocument();
  });
});
