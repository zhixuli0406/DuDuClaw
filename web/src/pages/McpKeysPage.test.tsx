import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MCP_SCOPES } from '@/lib/api';
import { McpKeysPage } from './McpKeysPage';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('McpKeysPage', () => {
  it('shows the empty state when no keys exist', async () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    expect(await screen.findByText('No MCP keys yet')).toBeInTheDocument();
  });

  it('renders the create-key action', () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    expect(screen.getByRole('button', { name: /create key/i })).toBeInTheDocument();
  });

  // P0-S3 (2026-08 audit): MCP_SCOPES used to list only 10 of the 22 real
  // scopes, so 12 were simply not offered in this picker — an operator could
  // only grant them by hand-editing config.toml. This locks both halves of
  // the fix: every scope in MCP_SCOPES is offered as a checkbox, AND every
  // one resolves a real translated description (not a missing-key fallback
  // rendering the raw i18n id, which is exactly the failure mode a scope
  // added to the array without its `mcpKeys.scopeDesc.*` locale entry would
  // produce).
  it('offers a checkbox with a translated description for all 22 known scopes', async () => {
    mockWsClient.call.mockResolvedValue({ keys: [] });
    renderWithProviders(<McpKeysPage />);

    fireEvent.click(screen.getByRole('button', { name: /create key/i }));

    expect(MCP_SCOPES.length).toBe(22);

    // The dialog renders through a portal (appended to document.body, not
    // under the render container), so query via `screen` (document-scoped)
    // rather than a container-scoped selector.
    const checkboxes = await screen.findAllByRole('checkbox');
    expect(checkboxes).toHaveLength(MCP_SCOPES.length);

    // Read `aria-label` directly rather than matching the computed
    // accessible `name` — the checkbox is nested inside a `<label>` whose
    // own text also feeds the accname algorithm, so `getByRole(..., {name})`
    // would be matching against label-text-plus-description, not the scope
    // string alone.
    const labels = checkboxes.map((el) => el.getAttribute('aria-label')).sort();
    expect(labels).toEqual([...MCP_SCOPES].sort());

    for (const scope of MCP_SCOPES) {
      const missingKeyFallback = `mcpKeys.scopeDesc.${scope.replace(':', '.')}`;
      expect(screen.queryByText(missingKeyFallback)).not.toBeInTheDocument();
    }
  });
});
