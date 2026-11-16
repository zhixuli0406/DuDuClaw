import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { McpStartOutcome, ServerRow } from './McpPage';

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

describe('MCP check now', () => {
  beforeEach(() => { vi.clearAllMocks(); });
  const entry = { name: 'fixture', command: 'fixture-http-server', args: [], env: {}, source: 'mcp_json' as const };
  it.each(['identity_rejected', 'timeout', 'busy', null] as const)('checks the selected server and shows %s', async (reason) => {
    let resolve!: (value: unknown) => void;
    mockWsClient.call.mockImplementation(() => new Promise((done) => { resolve = done; }));
    renderWithProviders(<ServerRow entry={entry} agentId="alice" canEdit onEdit={vi.fn()} onRemove={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Check now' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('mcp.probe', { agent_id: 'alice', server_name: 'fixture' });
    expect(screen.getByRole('button', { name: 'Checking…' })).toBeDisabled();
    resolve({ ok: reason === null, reason_code: reason, tool_count: reason === null ? 2 : null });
    const messages = {
      identity_rejected: /Caller identity rejected/,
      timeout: /Start probe timed out/,
      busy: /Server check busy/,
      success: /Connected/,
    };
    await waitFor(() => expect(screen.getByText(messages[reason ?? 'success'])).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Check now' })).toBeEnabled();
  });
  it('does not offer a check to non-admins or read-only entries', () => {
    renderWithProviders(<ServerRow entry={{ ...entry, source: 'agent_toml', read_only: true }}
      agentId="alice" canEdit={false} onEdit={vi.fn()} onRemove={vi.fn()} />);
    expect(screen.queryByRole('button', { name: 'Check now' })).not.toBeInTheDocument();
  });
});
