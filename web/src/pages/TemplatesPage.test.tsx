import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { navGroups, personalPrimaryItems } from '@/components/layout/nav-model';
import { TemplatesPage } from './TemplatesPage';

const builtin = { id: 'builtin:assistant', name: 'Office assistant', description: 'Prepare quotations', kind: 'agent', source: 'builtin' };
const company = { id: 'company:123', name: 'Company assistant', description: 'Company workflow', kind: 'agent', source: 'company' };
const team = { id: 'team:office', name: 'Office team', description: 'A coordinated team', kind: 'team', source: 'builtin' };
const detail = {
  ...builtin, soul_md: 'Patient and precise', contract_toml: 'purpose = "Help customers"', skill_contents: { quotation: 'Quote only reviewed prices', research: 'Cite sources' }, skills: ['quotation', 'research'],
  allowed_tools: ['Read', 'Write'], denied_tools: ['Bash'], model: 'configured-model', runtime: 'configured-provider',
};
const fetchAgents = vi.fn().mockResolvedValue(undefined);

function renderPage() {
  return render(<IntlProvider locale="en" messages={en}><MemoryRouter><TemplatesPage /></MemoryRouter></IntlProvider>);
}

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } } as never);
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } } as never);
  useAgentsStore.setState({ agents: [{ name: 'existing', display_name: 'Existing assistant' }], fetchAgents } as never);
  mockWsClient.call.mockImplementation(async (method: string, params?: { id?: string; preview?: boolean }) => {
    switch (method) {
      case 'templates.catalog': return { templates: [builtin, company, team] };
      case 'templates.get': return params?.id === company.id ? { ...detail, ...company, revision: 4 }
        : params?.id === team.id ? { ...detail, ...team, members: [{ ...detail, id: 'member-0', name: 'Office assistant', description: '' }] } : detail;
      case 'users.list': return { users: [{ id: 'user-1', display_name: 'Alice', status: 'active' }] };
      case 'departments.list': return { departments: [{ name: 'Sales' }] };
      case 'templates.apply': return { success: true, agents: ['new-assistant'] };
      case 'templates.copy': return params?.preview ? { preview: detail, review_digest: 'review-digest' } : { id: company.id };
      case 'templates.save': return { ...detail, ...company, revision: 5 };
      default: throw new Error(`Unexpected method ${method}`);
    }
  });
});

describe('TemplatesPage', () => {
  it('searches templates and shows reviewed detail without editable built-in fields', async () => {
    const user = userEvent.setup();
    renderPage();
    await screen.findByRole('button', { name: /Office assistant/ });
    await user.type(screen.getByRole('textbox', { name: 'Search templates' }), 'quotation');
    expect(screen.queryByRole('button', { name: /Company assistant/ })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Office assistant/ }));
    expect(await screen.findByText('Patient and precise')).toBeInTheDocument();
    expect(screen.getByText('configured-model')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Copy to edit' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Edit company template' })).not.toBeInTheDocument();
  });

  it('collects identity and bindings, and only creates after review', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Apply' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^Identifier/), 'new-assistant');
    await user.type(within(dialog).getByLabelText('Display name'), 'New assistant');
    await user.click(await within(dialog).findByRole('checkbox', { name: 'Alice' }));
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    expect(mockWsClient.call).not.toHaveBeenCalledWith('templates.apply', expect.anything());
    await user.click(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Create employees' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.apply', {
      id: builtin.id, name: 'new-assistant', display_name: 'New assistant', department: '', bound_users: ['user-1'], reviewed: true,
    }));
    expect(await within(dialog).findByRole('link', { name: 'new-assistant' })).toHaveAttribute('href', '/agents/new-assistant');
  });


  it('applies the reviewed company revision', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Company assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Apply' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^Identifier/), 'new-assistant');
    await user.type(within(dialog).getByLabelText('Display name'), 'New assistant');
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeEnabled());
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Create employees' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.apply', expect.objectContaining({ id: company.id, revision: 4 })));
  });

  it('keeps partial team results visible and does not offer a blind retry', async () => {
    const implementation = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, params) => method === 'templates.apply'
      ? Promise.resolve({ success: false, agents: ['new-assistant'] }) : implementation(method, params));
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office team/ }));
    await user.click(await screen.findByRole('button', { name: 'Apply' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^Identifier/), 'new');
    await user.type(within(dialog).getByLabelText('Display name'), 'New team');
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeEnabled());
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Create employees' }));
    expect(await within(dialog).findByText(en['templates.partial'])).toBeInTheDocument();
    expect(within(dialog).queryByRole('button', { name: 'Create employees' })).not.toBeInTheDocument();
  });

  it('sends the company revision and narrowed skills when saving', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Company assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Edit company template' }));
    const dialog = screen.getByRole('dialog');
    await user.click(within(dialog).getByRole('checkbox', { name: 'research' }));
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(await within(dialog).findByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.save', expect.objectContaining({
      id: company.id, revision: 4, reviewed: true, skills: ['quotation'], allowed_tools: ['Read', 'Write'], denied_tools: ['Bash'],
    })));
  });


  it('exports only the selected employee and template metadata', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole('button', { name: 'Save employee as company template' }));
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeDisabled();
    await user.click(within(dialog).getByRole('combobox', { name: 'Choose AI employee' }));
    await user.click(await screen.findByRole('option', { name: 'Existing assistant' }));
    await user.type(within(dialog).getByLabelText('Template name'), 'Shared assistant');
    await user.type(within(dialog).getByLabelText('Description'), 'Reviewed workflow');
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(await within(dialog).findByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.copy', {
      agent: 'existing', name: 'Shared assistant', description: 'Reviewed workflow', reviewed: true, review_digest: 'review-digest',
    }));
  });

  it('does not fetch enterprise binding data on personal editions', async () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } } as never);
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Apply' }));
    expect(screen.queryByText('People with access')).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('users.list');
    expect(mockWsClient.call).not.toHaveBeenCalledWith('departments.list');
  });

  it('refuses non-admin page access before loading template data', () => {
    useAuthStore.setState({ user: { id: 'employee', role: 'employee' } } as never);
    renderPage();
    expect(screen.getByText(en['templates.adminOnly'])).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });

  it('copies a whole built-in team before editing', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office team/ }));
    await user.click(await screen.findByRole('button', { name: 'Copy to edit' }));
    const dialog = screen.getByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(await within(dialog).findByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.copy', expect.objectContaining({ template_id: team.id })));
  });

  it('exports the selected team root', async () => {
    const user = userEvent.setup();
    renderPage();
    await user.click(screen.getByRole('button', { name: en['templates.exportTeam'] }));
    const dialog = screen.getByRole('dialog');
    await user.click(within(dialog).getByRole('combobox', { name: en['templates.teamRoot'] }));
    await user.click(await screen.findByRole('option', { name: 'Existing assistant' }));
    await user.type(within(dialog).getByLabelText('Template name'), 'Shared team');
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(await within(dialog).findByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.copy', { team_agent: 'existing', name: 'Shared team', description: '', reviewed: true, review_digest: 'review-digest' }));
  });

  it('edits company team members with the reviewed revision', async () => {
    const implementation = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, params) => method === 'templates.get' && (params as { id: string }).id === company.id
      ? Promise.resolve({ ...detail, ...company, kind: 'team', revision: 4, members: [{ ...detail, id: 'member-0' }] }) : implementation(method, params));
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Company assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Edit company template' }));
    const dialog = screen.getByRole('dialog');
    await user.click(within(dialog).getByRole('checkbox', { name: 'research' }));
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(await within(dialog).findByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.save', expect.objectContaining({ revision: 4,
      members: [expect.objectContaining({ id: 'member-0', edit: expect.objectContaining({ skills: ['quotation'] }) })],
    })));
  });

  it('shows full raw documents and requires an explicit review acknowledgement', async () => {
    const implementation = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, params) => method === 'templates.get'
      ? Promise.resolve({ ...detail, skill_contents: { quotation: '<!-- raw review text -->', research: 'Cite sources' } }) : implementation(method, params));
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office assistant/ }));
    expect(await screen.findByText('<!-- raw review text -->')).toBeInTheDocument();
    expect(screen.getByText(detail.contract_toml)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Apply' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^Identifier/), 'reviewed-agent');
    await user.type(within(dialog).getByLabelText('Display name'), 'Reviewed');
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeEnabled());
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    expect(within(dialog).getByText(detail.contract_toml)).toBeInTheDocument();
    expect(within(dialog).getByText('<!-- raw review text -->')).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Create employees' })).toBeDisabled();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('templates.apply', expect.anything());
    await user.click(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Back' }));
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    expect(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] })).not.toBeChecked();
    expect(within(dialog).getByRole('button', { name: 'Create employees' })).toBeDisabled();
  });

  it('previews a copy without saving and discards a late preview after metadata changes', async () => {
    const implementation = mockWsClient.call.getMockImplementation()!;
    let resolvePreview!: (value: unknown) => void;
    mockWsClient.call.mockImplementation((method, params) => method === 'templates.copy'
      ? new Promise((resolve) => { resolvePreview = resolve; }) : implementation(method, params));
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Copy to edit' }));
    const dialog = screen.getByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('templates.copy', expect.objectContaining({ preview: true })));
    await user.type(within(dialog).getByLabelText('Template name'), ' changed');
    resolvePreview({ preview: detail, review_digest: 'stale' });
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeEnabled());
    expect(within(dialog).queryByRole('checkbox', { name: en['templates.reviewed'] })).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'templates.copy')).toHaveLength(1);
  });

  it('explains a mid-team revision change while keeping created employees visible', async () => {
    const implementation = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, params) => method === 'templates.apply'
      ? Promise.resolve({ success: false, agents: ['created-member'], warning: 'template_changed' }) : implementation(method, params));
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office team/ }));
    await user.click(await screen.findByRole('button', { name: 'Apply' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^Identifier/), 'new-team');
    await user.type(within(dialog).getByLabelText('Display name'), 'New team');
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeEnabled());
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Create employees' }));
    expect(await within(dialog).findByText(en['templates.templateChanged'])).toBeInTheDocument();
    expect(within(dialog).getByRole('link', { name: 'created-member' })).toBeInTheDocument();
    expect(within(dialog).queryByRole('button', { name: 'Create employees' })).not.toBeInTheDocument();
  });

  it('shows the rejecting file and server scan message after apply fails', async () => {
    const error = 'SKILLS/writing/SKILL.md 含隱藏內容或提示注入模式，已拒絕複製';
    const implementation = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, params) => method === 'templates.apply'
      ? Promise.resolve({ success: false, agents: [], warning: 'creation_failed', error }) : implementation(method, params));
    const user = userEvent.setup();
    renderPage();
    await user.click(await screen.findByRole('button', { name: /Office assistant/ }));
    await user.click(await screen.findByRole('button', { name: 'Apply' }));
    const dialog = screen.getByRole('dialog');
    await user.type(within(dialog).getByLabelText(/^Identifier/), 'new-assistant');
    await user.type(within(dialog).getByLabelText('Display name'), 'New assistant');
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Next: review' })).toBeEnabled());
    await user.click(within(dialog).getByRole('button', { name: 'Next: review' }));
    await user.click(within(dialog).getByRole('checkbox', { name: en['templates.reviewed'] }));
    await user.click(within(dialog).getByRole('button', { name: 'Create employees' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(error);
    expect(within(dialog).queryByRole('button', { name: 'Create employees' })).not.toBeInTheDocument();
  });

  it('provides admin navigation and a release badge on both editions', () => {
    const enterprise = navGroups.flatMap((group) => group.items).find((item) => item.to === '/templates');
    const personal = personalPrimaryItems.find((item) => item.to === '/templates');
    expect(enterprise).toMatchObject({ minRole: 'admin', newIn: '1.80.0' });
    expect(personal).toMatchObject({ minRole: 'admin', newIn: '1.80.0' });
  });
});
