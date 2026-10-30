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
    expect(screen.getByRole('heading', { name: 'Reviewed name' })).toBeInTheDocument();
    expect(screen.getByText('<img src=x onerror=bad()>')).toBeInTheDocument();
    expect(screen.getByText('role: Descriptive only')).toBeInTheDocument();
    expect(screen.getByText(/Review required \(90\): injection/)).toBeInTheDocument();
  });

  it('shows resolved identity, model, tools, supervisor, settings and file hashes', () => {
    const hash = 'a'.repeat(64);
    renderWithProviders(<AgentProfilePreview profiles={[{
      id: 'advisor-imported', name: 'Advisor (imported)',
      model: 'claude-sonnet-4-6', reports_to: 'team-lead',
      tools: ['Read', 'Grep', 'mcp__duduclaw__*'], disallowed_tools: ['Bash', 'Write'],
      persona: { soul: 'Persona', identity: '' }, user: '', rules: '', contract: '',
      settings: { capabilities: { allowed_tools: ['Read', 'Grep', 'mcp__duduclaw__*'], denied_tools: ['Bash', 'Write'] } },
      files: [{ path: 'agents/advisor-imported/agent.toml', sha256: hash }],
      input_guard: [],
    }]} />);
    expect(screen.getByText('advisor-imported')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Advisor (imported)' })).toBeInTheDocument();
    expect(screen.getByText('claude-sonnet-4-6')).toBeInTheDocument();
    expect(screen.getByText('team-lead')).toBeInTheDocument();
    expect(screen.getByText('Read, Grep, mcp__duduclaw__*')).toBeInTheDocument();
    expect(screen.getByText('Bash, Write')).toBeInTheDocument();
    expect(screen.getByText(/"allowed_tools"/)).toHaveTextContent('"denied_tools"');
    expect(screen.getByText('agents/advisor-imported/agent.toml')).toBeInTheDocument();
    expect(screen.getByText(`SHA-256: ${hash}`)).toBeInTheDocument();
  });

  it.each([{ tools: null }, { tools: [] }])('explains default access for tools=$tools without hiding denied tools', ({ tools }) => {
    renderWithProviders(<AgentProfilePreview profiles={[{
      id: 'helper', model: null, reports_to: null, tools, disallowed_tools: ['Bash'],
      persona: { soul: '', identity: '' }, user: '', rules: '', contract: '', input_guard: [],
    }]} />);
    expect(screen.getByText('Default model')).toBeInTheDocument();
    expect(screen.getByText('No supervisor (root)')).toBeInTheDocument();
    expect(screen.getByText('Default tool access; no additional allowlist restriction. Denied tools still apply.')).toBeInTheDocument();
    expect(screen.getByText('Bash')).toBeInTheDocument();
  });

  it('shows every installation effect outside employee profiles as inert text', () => {
    const hash = 'b'.repeat(64);
    const plan = {
      global_skills: [{ path: 'skills/review/SKILL.md', sha256: hash, disposition: 'enabled' }],
      commands: [{ path: 'skills/command/SKILL.md', sha256: hash, disposition: 'enabled' }],
      hooks: [{ path: 'imported/plugin/hooks/<script>bad()</script>', sha256: hash, disposition: 'disabled' }],
      archive: [{ path: 'imported/plugin/source/README.md', sha256: hash, disposition: 'archive' }],
    };
    const { container } = renderWithProviders(<AgentProfilePreview profiles={[]} plan={plan} />);
    expect(screen.getByRole('heading', { name: 'Complete installation plan' })).toBeInTheDocument();
    const rendered = screen.getByText(/"global_skills"/);
    for (const effect of Object.values(plan).flat()) {
      expect(rendered).toHaveTextContent(effect.path);
      expect(rendered).toHaveTextContent(effect.sha256);
      expect(rendered).toHaveTextContent(effect.disposition);
    }
    expect(container.querySelector('script')).toBeNull();
  });
});
