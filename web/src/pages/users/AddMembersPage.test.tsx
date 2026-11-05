import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { AddMembersPage } from './AddMembersPage';

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'agents.list') return Promise.resolve({ agents: [{ name: 'nova', display_name: 'Nova' }, { name: 'reader', display_name: 'Reader' }] });
    return Promise.resolve({ departments: [] });
  });
});
const renderPage = () => renderWithProviders(<AddMembersPage onBack={vi.fn()} onCreated={vi.fn()} />);
async function fillMember(number: number, email: string) {
  const user = userEvent.setup();
  const region = within(screen.getByRole('region', { name: `Member ${number}` }));
  await user.type(region.getByLabelText('Display name'), `Member ${number}`);
  await user.type(region.getByLabelText('Email address'), email);
  await user.click(region.getByRole('combobox', { name: 'Primary AI staff' }));
  await user.click(await screen.findByRole('option', { name: 'Nova' }));
  return { user, region };
}

describe('AddMembersPage', () => {
  it('creates an account with staff bindings and no password, with invitation enabled by default', async () => {
    renderPage();
    const { user, region } = await fillMember(1, 'first@example.test');
    expect(region.getByRole('checkbox')).toBeChecked();
    await user.click(region.getByRole('button', { name: 'Assign another AI staff member' }));
    await user.click(region.getByRole('combobox', { name: 'Additional AI staff' }));
    await user.click(await screen.findByRole('option', { name: 'Reader' }));
    expect(document.querySelector('input[type="password"]')).not.toBeInTheDocument();
    mockWsClient.call.mockResolvedValue({ results: [{ index: 0, user: { id: 'created' }, invite: { sent: true } }] });
    await user.click(screen.getByRole('button', { name: 'Create accounts and invitations' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('users.create_members', {
      public_url: window.location.origin, locale: 'en', members: [{
        display_name: 'Member 1', email: 'first@example.test', role: 'employee', department: undefined,
        primary_agent_id: 'nova', bindings: [{ agent_name: 'nova', access_level: 'operator' }, { agent_name: 'reader', access_level: 'viewer' }], invite: true,
      }],
    }, false, 330000);
    expect(await screen.findByText('Account and AI staff access created. Invitation sent.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Create accounts and invitations' })).not.toBeInTheDocument();
  });

  it('only offers Operator access for the primary employee', async () => {
    renderPage();
    const user = userEvent.setup();
    await user.click(screen.getByRole('combobox', { name: 'Access for AI staff 1' }));
    expect(await screen.findByRole('option', { name: 'Operator' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Viewer' })).not.toBeInTheDocument();
  });

  it('retries only failed rows and retains a created account after email delivery failure', async () => {
    renderPage();
    const { user } = await fillMember(1, 'first@example.test');
    await user.click(screen.getByRole('button', { name: 'Add another member (up to 10)' }));
    await fillMember(2, 'second@example.test');
    mockWsClient.call.mockResolvedValueOnce({ results: [
      { index: 0, user: { id: 'first' }, invite: { sent: false, link: 'https://example.test/invite#token' } },
      { index: 1, error: 'invalid_member' },
    ] });
    await user.click(screen.getByRole('button', { name: 'Create accounts and invitations' }));
    expect(await screen.findByText(/Account and AI staff access created, but the invitation was not sent/)).toBeInTheDocument();
    expect(screen.getByText(/This member was not created/)).toBeInTheDocument();
    mockWsClient.call.mockResolvedValueOnce({ results: [{ index: 0, user: { id: 'second' }, invite: { sent: true } }] });
    await user.click(screen.getByRole('button', { name: 'Create accounts and invitations' }));
    const calls = mockWsClient.call.mock.calls.filter(([method]) => method === 'users.create_members');
    expect(calls[1][1]).toMatchObject({ members: [{ email: 'second@example.test' }] });
    expect((calls[1][1] as { members: unknown[] }).members).toHaveLength(1);
  });

  it.each([
    ['member_email_exists', 'This email address is already in use. Use a different address or manage the existing account from the members list.'],
    ['member_invite_url_required', 'Set an HTTPS address that recipients can access before sending invitations.'],
    ['unexpected_internal_error: /private/server', 'This member was not created. Check whether the email is already in use and review the staff and department settings before retrying.'],
  ])('shows an actionable, translated row error for %s without exposing the raw code', async (code, message) => {
    renderPage();
    const { user } = await fillMember(1, 'first@example.test');
    mockWsClient.call.mockResolvedValueOnce({ results: [{ index: 0, error: code }] });
    await user.click(screen.getByRole('button', { name: 'Create accounts and invitations' }));
    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(screen.queryByText(code)).not.toBeInTheDocument();
    expect(screen.getByLabelText('Email address')).toHaveValue('first@example.test');
  });

  it('previews the server email in an inert frame without submitting accounts', async () => {
    renderPage();
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValueOnce({ subject: 'Invitation preview', html: '<p>Welcome</p>', text: 'Welcome', examples: [] });
    await user.click(screen.getByRole('button', { name: 'Preview invitation' }));
    const frame = await screen.findByTitle('Preview invitation');
    expect(frame).toHaveAttribute('sandbox', '');
    expect(frame).toHaveAttribute('srcdoc', '<p>Welcome</p>');
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'users.create_members')).toBe(false);
  });

  it('discards old account results and bearer links after the active administrator changes', async () => {
    const onCreated = vi.fn();
    renderWithProviders(<AddMembersPage onBack={vi.fn()} onCreated={onCreated} />);
    const { user } = await fillMember(1, 'first@example.test');
    let resolve!: (value: unknown) => void;
    mockWsClient.call.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    await user.click(screen.getByRole('button', { name: 'Create accounts and invitations' }));
    act(() => useAuthStore.setState({ user: { id: 'another-admin', role: 'admin' } as never }));
    await act(async () => resolve({ results: [{ index: 0, user: { id: 'old-result' }, invite: { sent: false, link: 'https://example.test/invite#old-token' } }] }));
    expect(onCreated).not.toHaveBeenCalled();
    expect(screen.getByLabelText('Email address')).toHaveValue('');
    expect(screen.queryByText(/Account and AI staff access created/)).not.toBeInTheDocument();
  });

  it('allows at most ten members per batch', async () => {
    renderPage();
    const user = userEvent.setup();
    const add = screen.getByRole('button', { name: 'Add another member (up to 10)' });
    for (let i = 1; i < 10; i++) await user.click(add);
    expect(add).toBeDisabled();
    expect(screen.getAllByRole('region', { name: /^Member / })).toHaveLength(10);
  });

  it('does not expose the creation form to a non-admin', () => {
    useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
    renderPage();
    expect(screen.getByRole('alert')).toHaveTextContent('Only administrators can add members.');
    expect(screen.queryByRole('button', { name: 'Create accounts and invitations' })).not.toBeInTheDocument();
  });
});
