import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

const getMock = vi.fn();
const setMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    configRaw: {
      get: (file: string) => getMock(file),
      set: (file: string, content: string, baseHash?: string) => setMock(file, content, baseHash),
    },
  },
}));

import { ConfigRawTab, parseRawError } from './ConfigRawTab';

const ORIGINAL = '[gateway]\nauth_token = "«set»"\nport = 3100\n';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useRestartRequiredStore.getState().clear();
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [] as never });
  getMock.mockResolvedValue({ content: ORIGINAL, hash: 'h1', hidden_sections: ['mcp_keys'] });
});

describe('<ConfigRawTab>', () => {
  it('round-trips masked secrets untouched and reports restart keys and the backup', async () => {
    setMock.mockResolvedValue({ success: true, restart_required: ['gateway.port'], backup: 'config.toml.bak-1' });
    renderWithProviders(<ConfigRawTab />);
    const editor = (await screen.findByTestId('config-raw-editor')) as HTMLTextAreaElement;
    await waitFor(() => expect(editor.value).toBe(ORIGINAL));
    const edited = ORIGINAL.replace('port = 3100', 'port = 3200');
    fireEvent.change(editor, { target: { value: edited } });
    fireEvent.click(screen.getByRole('button', { name: 'Check and save' }));
    await waitFor(() => expect(setMock).toHaveBeenCalled());
    expect(setMock).toHaveBeenCalledWith('config', edited, 'h1');
    expect(screen.getByText(/because their names are themselves secrets: mcp_keys/)).toBeInTheDocument();
    expect((setMock.mock.calls[0][1] as string)).toContain('auth_token = "«set»"');
    expect(await screen.findByText(/gateway\.port/)).toBeInTheDocument();
    expect(screen.getByText('Backup: config.toml.bak-1')).toBeInTheDocument();
    expect(useRestartRequiredStore.getState().keys).toEqual(['gateway.port']);
  });

  it('shows the line and column of a rejected file', async () => {
    setMock.mockRejectedValue(new Error('TOML parse error at line 3, column 8: expected a value'));
    renderWithProviders(<ConfigRawTab />);
    const editor = (await screen.findByTestId('config-raw-editor')) as HTMLTextAreaElement;
    await waitFor(() => expect(editor.value).toBe(ORIGINAL));
    fireEvent.change(editor, { target: { value: ORIGINAL + 'broken =\n' } });
    fireEvent.click(screen.getByRole('button', { name: 'Check and save' }));
    expect(await screen.findByText(/Problem at line 3, column 8/)).toBeInTheDocument();
  });

  it('tells an older gateway apart from a load failure', async () => {
    getMock.mockRejectedValue(new Error('Unknown method: config.raw.get'));
    renderWithProviders(<ConfigRawTab />);
    expect(await screen.findByText(/cannot edit configuration files from the dashboard yet/)).toBeInTheDocument();
  });
});

describe('parseRawError', () => {
  it('reads structured and textual positions', () => {
    expect(parseRawError({ message: 'bad', line: 4, column: 2 })).toEqual({ message: 'bad', line: 4, column: 2 });
    expect(parseRawError('line 9, column 1: oops')).toMatchObject({ line: 9, column: 1 });
    expect(parseRawError(new Error('no position'))).toEqual({ message: 'no position', line: undefined, column: undefined });
  });
});
