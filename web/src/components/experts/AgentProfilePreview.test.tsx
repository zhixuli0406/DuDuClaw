import { describe, expect, it } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AgentProfilePreview } from './AgentProfilePreview';

describe('AgentProfilePreview', () => {
  it('shows all four sections and input checks as inert text', () => {
    const { container } = renderWithProviders(<AgentProfilePreview profiles={[{
      id: 'a1', name: 'Reviewed name', metadata: { description: '<img src=x onerror=bad()>' }, unmapped_fields: [['role', 'Descriptive only']], persona: { soul: 'Kind persona', identity: 'Support identity' },
      user: '<script>bad()</script>', rules: 'Working rules', contract: 'Never disclose secrets',
      input_guard: [{ section: 'user', blocked: true, matched_rules: ['injection'], score: 90 }],
    }]} />);
    for (const name of ['Persona', 'User', 'Rules', 'Contract', 'Input checks']) {
      expect(screen.getByRole('heading', { name })).toBeInTheDocument();
    }
    expect(screen.getByText('<script>bad()</script>')).toBeInTheDocument();
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('img')).toBeNull();
    expect(screen.getByText('Reviewed name')).toBeInTheDocument();
    expect(screen.getByText('<img src=x onerror=bad()>')).toBeInTheDocument();
    expect(screen.getByText('role: Descriptive only')).toBeInTheDocument();
    expect(screen.getByText(/Review required \(90\): injection/)).toBeInTheDocument();
  });
});
