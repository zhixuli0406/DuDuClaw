import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { McpPage } from './McpPage';

const BROWSERBASE = {
  id: 'browserbase', name: 'Browserbase', description: 'Remote browser', category: 'browser',
  requires_oauth: false,
  default_def: { command: 'npx', args: ['-y', '@browserbasehq/mcp'], env: { BROWSERBASE_API_KEY: 'not_set' } },
  required_env: ['BROWSERBASE_API_KEY'],
};
const PLAYWRIGHT = {
  id: 'playwright', name: 'Playwright', description: 'Browser automation', category: 'browser',
  requires_oauth: false,
  default_def: { command: 'npx', args: ['-y', '@playwright/mcp@0.0.41', '--headless'], env: {} },
  required_env: [],
};

function mockList(agents: unknown[]) {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'mcp.list') return { agents, catalog: [BROWSERBASE, PLAYWRIGHT] };
    if (method === 'marketplace.install') return { success: true, agent_id: 'sam' };
    if (method === 'mcp.update') return { success: true };
    return { providers: [] };
  });
}

function renderPage(tab: 'marketplace' | 'agents') {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[`/?mcp_tab=${tab}`]}>
        <McpPage />
      </MemoryRouter>
    </IntlProvider>,
  );
}

function callParams(method: string) {
  return mockWsClient.call.mock.calls.find(([m]) => m === method)?.[1];
}

/** Open a marketplace card's install button (the card list, not the header). */
async function openCardInstall(name: string) {
  const row = (await screen.findByText(name)).closest('div.flex.items-center.gap-3') as HTMLElement;
  fireEvent.click(within(row).getByRole('button', { name: /Install to Agent/ }));
  return screen.findByRole('dialog');
}

async function pickAgent(dialog: HTMLElement, user: ReturnType<typeof userEvent.setup>, label: string) {
  const triggers = within(dialog).getAllByRole('combobox');
  // The agent picker is the last select in the dialog (catalogue picker first).
  await user.click(triggers[triggers.length - 1]);
  await user.click(await screen.findByRole('option', { name: label }));
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  useAuthStore.setState({ user: { role: 'admin' } } as never);
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [{ name: 'sam', display_name: 'Sam' }] as never });
  mockList([]);
});

// v1.68 (W2): mcp.list answers env values masked (set / not_set /
// reference). Installing from the marketplace must never send those words
// back as values; required names are typed by the operator.
describe('McpPage marketplace install', () => {
  it('installs through marketplace.install with the typed key, never the masked word', async () => {
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Browserbase');
    await pickAgent(dialog, user, 'Sam');
    const confirm = within(dialog).getByRole('button', { name: 'Add MCP Server' });
    fireEvent.click(confirm);
    // Missing required key → refused in the dialog, nothing sent.
    expect(await within(dialog).findByText(/BROWSERBASE_API_KEY/, { selector: 'span' })).toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'marketplace.install')).toBe(false);
    fireEvent.change(within(dialog).getByLabelText('BROWSERBASE_API_KEY'), { target: { value: 'bb-secret' } });
    fireEvent.click(confirm);
    await waitFor(() => expect(callParams('marketplace.install')).toBeDefined());
    expect(callParams('marketplace.install')).toEqual({ id: 'browserbase', agent_id: 'sam', env: { BROWSERBASE_API_KEY: 'bb-secret' } });
    expect(JSON.stringify(mockWsClient.call.mock.calls)).not.toContain('not_set"}}');
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'mcp.update')).toBe(false);
  });

  // 2026-10-30: the card used to open an agent-only dialog with no way to
  // change the arguments (a customer could not drop Playwright's --headless).
  it('opens the shared dialog with the card preselected and editable arguments', async () => {
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Playwright');
    const argsField = within(dialog).getByLabelText('Arguments (space-separated)') as HTMLInputElement;
    expect(argsField.value).toBe('-y @playwright/mcp@0.0.41 --headless');
    await pickAgent(dialog, user, 'Sam');
    fireEvent.change(argsField, { target: { value: '-y @playwright/mcp@0.0.41' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Add MCP Server' }));
    await waitFor(() => expect(callParams('mcp.update')).toBeDefined());
    expect(callParams('mcp.update')).toMatchObject({
      agent_id: 'sam',
      action: 'add',
      server_name: 'playwright',
      server_def: { command: 'npx', args: ['-y', '@playwright/mcp@0.0.41'] },
    });
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'marketplace.install')).toBe(false);
  });

  it('keeps marketplace.install for an untouched card install', async () => {
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Playwright');
    await pickAgent(dialog, user, 'Sam');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Add MCP Server' }));
    await waitFor(() => expect(callParams('marketplace.install')).toBeDefined());
    expect(callParams('marketplace.install')).toEqual({ id: 'playwright', agent_id: 'sam' });
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'mcp.update')).toBe(false);
  });
});

describe('McpPage editing an installed server', () => {
  const INSTALLED = [{
    agent_id: 'sam',
    servers: [{
      name: 'browserbase', command: 'npx', args: ['-y', '@browserbasehq/mcp', '--headless'],
      env: { BROWSERBASE_API_KEY: 'set', PROJECT: 'reference' },
    }],
  }];

  it('prefills from the installed definition and saves through mcp.update, keeping stored secrets', async () => {
    mockList(INSTALLED);
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Browserbase');
    await pickAgent(dialog, user, 'Sam');
    const argsField = within(dialog).getByLabelText('Arguments (space-separated)') as HTMLInputElement;
    await waitFor(() => expect(argsField.value).toBe('-y @browserbasehq/mcp --headless'));
    expect(within(dialog).getByText(/already has browserbase/)).toBeInTheDocument();
    // The stored key is kept, so it is not asked for again.
    expect(within(dialog).queryByLabelText('BROWSERBASE_API_KEY')).toBeNull();
    const save = within(dialog).getByRole('button', { name: 'Save changes' });

    // A `${…}` reference cannot be carried over: saving is refused until re-entered.
    fireEvent.click(save);
    expect(await within(dialog).findByText(/Enter these values first: PROJECT/)).toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'mcp.update')).toBe(false);

    fireEvent.change(argsField, { target: { value: '-y @browserbasehq/mcp' } });
    fireEvent.change(within(dialog).getByLabelText(/Environment Variables/), {
      target: { value: 'BROWSERBASE_API_KEY=set\nPROJECT=${BB_PROJECT}' },
    });
    fireEvent.click(save);
    await waitFor(() => expect(callParams('mcp.update')).toBeDefined());
    expect(callParams('mcp.update')).toEqual({
      agent_id: 'sam',
      action: 'add',
      server_name: 'browserbase',
      server_def: {
        command: 'npx',
        args: ['-y', '@browserbasehq/mcp'],
        env: { BROWSERBASE_API_KEY: 'set', PROJECT: '${BB_PROJECT}' },
      },
    });
    expect(mockWsClient.call.mock.calls.some(([m]) => m === 'marketplace.install')).toBe(false);
  });

  // Review M1: a required key stored as a `${…}` reference is asked for in
  // the required-value field only, and that field alone satisfies it.
  it('asks for a required key stored as a reference once, in the required field', async () => {
    mockList([{
      agent_id: 'sam',
      servers: [{ name: 'browserbase', command: 'npx', args: ['-y', '@browserbasehq/mcp'], env: { BROWSERBASE_API_KEY: 'reference' } }],
    }]);
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Browserbase');
    await pickAgent(dialog, user, 'Sam');
    await within(dialog).findByText(/already has browserbase/);
    const envField = within(dialog).getByLabelText(/Environment Variables/) as HTMLTextAreaElement;
    expect(envField.value).not.toContain('BROWSERBASE_API_KEY');
    fireEvent.change(within(dialog).getByLabelText('BROWSERBASE_API_KEY'), { target: { value: 'bb-new' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(callParams('mcp.update')).toBeDefined());
    expect(callParams('mcp.update').server_def.env).toEqual({ BROWSERBASE_API_KEY: 'bb-new' });
  });

  // Review L1: deleting a stored `NAME=set` line is never silent.
  it('asks again for a required key whose stored line was deleted, and warns about dropped values', async () => {
    mockList(INSTALLED);
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Browserbase');
    await pickAgent(dialog, user, 'Sam');
    await within(dialog).findByText(/already has browserbase/);
    expect(within(dialog).queryByLabelText('BROWSERBASE_API_KEY')).toBeNull();
    fireEvent.change(within(dialog).getByLabelText(/Environment Variables/), { target: { value: 'PROJECT=${P}' } });
    expect(within(dialog).getByLabelText('BROWSERBASE_API_KEY')).toBeInTheDocument();

    fireEvent.change(within(dialog).getByLabelText(/Environment Variables/), {
      target: { value: 'BROWSERBASE_API_KEY=set' },
    });
    expect(within(dialog).getByText(/Saving removes these stored values: PROJECT/)).toBeInTheDocument();
  });

  // Review L2: edits made before choosing an agent survive that choice.
  it('keeps arguments edited before picking an agent that already has the server', async () => {
    mockList([{
      agent_id: 'sam',
      servers: [{ name: 'playwright', command: 'npx', args: ['-y', '@playwright/mcp@0.0.41', '--headless'], env: {} }],
    }]);
    const user = userEvent.setup();
    renderPage('marketplace');
    const dialog = await openCardInstall('Playwright');
    const argsField = within(dialog).getByLabelText('Arguments (space-separated)') as HTMLInputElement;
    fireEvent.change(argsField, { target: { value: '-y @playwright/mcp@0.0.41' } });
    await pickAgent(dialog, user, 'Sam');
    await within(dialog).findByText(/already has playwright/);
    expect(argsField.value).toBe('-y @playwright/mcp@0.0.41');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(callParams('mcp.update')).toBeDefined());
    expect(callParams('mcp.update').server_def.args).toEqual(['-y', '@playwright/mcp@0.0.41']);
  });

  // Review M2: typing an installed name in custom mode does not overwrite the form.
  it('does not overwrite custom-mode fields when the typed name matches an installed server', async () => {
    mockList(INSTALLED);
    const user = userEvent.setup();
    renderPage('marketplace');
    fireEvent.click(await screen.findByRole('button', { name: /^Add MCP Server$/ }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('radio', { name: 'Custom' }));
    await pickAgent(dialog, user, 'Sam');
    fireEvent.change(within(dialog).getByLabelText('Command'), { target: { value: 'uvx' } });
    fireEvent.change(within(dialog).getByPlaceholderText('e.g. filesystem'), { target: { value: 'browserbase' } });
    expect(await within(dialog).findByText(/already has browserbase/)).toBeInTheDocument();
    expect((within(dialog).getByLabelText('Command') as HTMLInputElement).value).toBe('uvx');
  });

  it('starts empty again on the next opening', async () => {
    mockList([]);
    const user = userEvent.setup();
    renderPage('marketplace');
    let dialog = await openCardInstall('Playwright');
    await pickAgent(dialog, user, 'Sam');
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    fireEvent.click(screen.getByRole('button', { name: /^Add MCP Server$/ }));
    dialog = await screen.findByRole('dialog');
    expect((within(dialog).getByLabelText('Command') as HTMLInputElement).value).toBe('');
    expect((within(dialog).getByLabelText('Arguments (space-separated)') as HTMLInputElement).value).toBe('');
  });

  it('the row 編輯 action opens the dialog locked to that agent and server', async () => {
    mockList(INSTALLED);
    const user = userEvent.setup();
    renderPage('agents');
    await user.click(await screen.findByRole('button', { name: 'More' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Edit' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('heading', { name: 'Edit MCP Server' })).toBeInTheDocument();
    const argsField = within(dialog).getByLabelText('Arguments (space-separated)') as HTMLInputElement;
    expect(argsField.value).toBe('-y @browserbasehq/mcp --headless');
    // Locked: no mode toggle / catalogue picker; agent picker disabled; name read-only.
    expect(within(dialog).queryByRole('radiogroup')).toBeNull();
    const combos = within(dialog).getAllByRole('combobox');
    expect(combos).toHaveLength(1);
    expect(combos[0]).toHaveAttribute('data-disabled');
    expect(within(dialog).getByDisplayValue('browserbase')).toHaveAttribute('readonly');
  });
});
