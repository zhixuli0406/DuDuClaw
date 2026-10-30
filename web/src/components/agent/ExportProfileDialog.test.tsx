import { afterEach, describe, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';

vi.mock('@/lib/ws-client', () => ({ client: { call: vi.fn(), subscribe: vi.fn() } }));
import { client } from '@/lib/ws-client';
import { ExportProfileDialog } from './ExportProfileDialog';

afterEach(() => vi.mocked(client.call).mockReset());

describe('ExportProfileDialog', () => {
  it('excludes user introduction until explicitly selected and exposes individual downloads', async () => {
    vi.mocked(client.call).mockResolvedValue({ files: [{ name: 'SOUL.md', content: 'Persona' }, { name: 'AGENTS.md', content: 'Rules' }] });
    renderWithProviders(<ExportProfileDialog agentId="a1" onClose={vi.fn()} />);
    const include = screen.getByRole('checkbox', { name: en['agentProfile.export.includeUser'] });
    expect(include).not.toBeChecked();
    await userEvent.click(screen.getByRole('button', { name: en['agentProfile.export.prepare'] }));
    expect(client.call).toHaveBeenCalledWith('agents.export_profile', { agent_id: 'a1', format: 'duduclaw', include_user: false });
    expect(await screen.findByRole('button', { name: 'SOUL.md' })).toBeInTheDocument();
    expect(screen.getByText(en['agentProfile.export.folderHint'])).toBeInTheDocument();
    await userEvent.click(include);
    expect(screen.queryByRole('button', { name: 'SOUL.md' })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: en['agentProfile.export.prepare'] }));
    expect(client.call).toHaveBeenLastCalledWith('agents.export_profile', { agent_id: 'a1', format: 'duduclaw', include_user: true });
  });

  it('shows a refused export without download links', async () => {
    vi.mocked(client.call).mockRejectedValue(new Error('Secret-shaped content in section Persona'));
    renderWithProviders(<ExportProfileDialog agentId="a1" onClose={vi.fn()} />);
    await userEvent.click(screen.getByRole('button', { name: en['agentProfile.export.prepare'] }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByText(en['agentProfile.export.ready'])).not.toBeInTheDocument();
  });
});
