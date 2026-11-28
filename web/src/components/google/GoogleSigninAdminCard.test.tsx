import { beforeEach, describe, expect, it, vi, type MockInstance } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { api } from '@/lib/api';
import { googleSignin, type GoogleSigninStatus } from '@/lib/google-signin';
import { templateLibrary, type LibraryTemplateDetail } from '@/lib/template-library';
import { GoogleSigninAdminCard } from './GoogleSigninAdminCard';

const TOKEN = Array(32).fill(7);
const BASE: GoogleSigninStatus = {
  settings: { enabled: false, enabled_requested: false, allowed_hd: ['example.com'], approval: 'admin', daily_new_accounts: 20,
    avatar_template: '', avatar_template_revision: null, approval_grants_mfa_enrollment: false, ack: null },
  redirect_uri: 'https://office.example.com/api/login/google/callback',
  checks: { client_configured: true, public_url_ok: false, domains_set: true, mfa_policy: 'not_enforced', personal_edition: false, personal_connection_domains_cover: null },
};
const TEMPLATE = { id: 'company:sales', kind: 'agent', source: 'company', name: 'Sales assistant', revision: 3,
  permissions: { can_schedule_tasks: true, can_create_agents: false, unsafe_unknown: ['hidden-tool'] } } as unknown as LibraryTemplateDetail;
let state: GoogleSigninStatus;
let get: MockInstance<typeof googleSignin.get>;
let set: MockInstance<typeof googleSignin.set>;
beforeEach(() => {
  vi.restoreAllMocks(); mockWsClient.call.mockReset();
  state = structuredClone(BASE);
  get = vi.spyOn(googleSignin, 'get').mockImplementation(async () => structuredClone(state));
  set = vi.spyOn(googleSignin, 'set').mockImplementation(async params => {
    state.settings = { ...state.settings, ...params, avatar_template_revision: params.avatar_template_revision ?? null, enabled_requested: params.enabled };
    return { settings: structuredClone(state.settings) };
  });
  vi.spyOn(templateLibrary, 'catalog').mockResolvedValue({ templates: [TEMPLATE, { ...TEMPLATE, id: 'builtin:other', source: 'builtin', name: 'Hidden built-in' }] });
  vi.spyOn(templateLibrary, 'get').mockResolvedValue(TEMPLATE);
  vi.spyOn(api.users, 'list').mockResolvedValue({ users: [{ id: 'secret-user-id', display_name: 'Alice', email: 'alice@example.com' }] } as never);
  mockWsClient.call.mockResolvedValue({ verified: true, step_up_csrf: TOKEN });
});
async function card() {
  renderWithProviders(<GoogleSigninAdminCard />);
  const scope = within(screen.getByTestId('google-signin-admin'));
  await scope.findByRole('switch');
  return scope;
}
async function risk() {
  const dialog = within(await screen.findByRole('dialog', { name: 'Enable company Google sign-in?' }));
  await userEvent.click(dialog.getByRole('checkbox', { name: 'I understand the effects of enabling this' }));
  await userEvent.click(dialog.getByRole('button', { name: 'Enable' }));
}
async function stepUp() {
  const dialog = within(await screen.findByRole('dialog'));
  await userEvent.type(dialog.getByLabelText('Current password'), 'password');
  await userEvent.type(dialog.getByLabelText('Verification code'), '123456');
  await userEvent.click(dialog.getByRole('button', { name: 'Verify and continue' }));
}
async function cap(scope: Awaited<ReturnType<typeof card>>, value = '21') {
  const input = scope.getByRole('spinbutton', { name: 'Daily new account limit' });
  await userEvent.clear(input); await userEvent.type(input, value);
  await userEvent.click(scope.getByRole('button', { name: 'Save' }));
}

describe('S-11 company Google sign-in administration', () => {
  it('defaults off with the server checklist, public URL link and redirect copy', async () => {
    const scope = await card();
    expect(scope.getByRole('switch')).not.toBeChecked();
    expect(scope.getByText('Google client configured · Passed')).toBeInTheDocument();
    expect(scope.getByText(/Company requires two-step verification.*Not passed/)).toBeInTheDocument();
    expect(scope.getByText(/Personal connection policy.*Unable to confirm/)).toBeInTheDocument();
    expect(scope.getByRole('link', { name: 'Configure the public URL' })).toHaveAttribute('href', '/app/system/settings?tab=general');
    expect(scope.getByText('Without a public URL, the sign-in page will not show the Google button.')).toBeInTheDocument();
    const copy = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: copy } });
    await userEvent.click(scope.getByRole('button', { name: 'Copy' }));
    expect(copy).toHaveBeenCalledWith(BASE.redirect_uri);
  });
  it('blocks an empty domain list before risk confirmation', async () => {
    state.settings.allowed_hd = [];
    const scope = await card(); await userEvent.click(scope.getByRole('switch'));
    expect(scope.getByRole('alert')).toHaveTextContent('Add at least one allowed domain before enabling sign-in.');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument(); expect(set).not.toHaveBeenCalled();
  });
  it('opens risk then step-up and sends acknowledgement and CSRF', async () => {
    const scope = await card(); await userEvent.click(scope.getByRole('switch'));
    expect(set).not.toHaveBeenCalled(); expect(mockWsClient.call).not.toHaveBeenCalled();
    await risk(); expect(set).not.toHaveBeenCalled();
    await stepUp();
    await waitFor(() => expect(set).toHaveBeenCalledWith(expect.objectContaining({ enabled: true, acknowledge_risk: true, step_up_csrf: TOKEN })));
    expect(get).toHaveBeenCalledTimes(2);
  });
  it('sends a pure disable without step-up or acknowledgement', async () => {
    state.settings.enabled = state.settings.enabled_requested = true;
    const scope = await card(); await userEvent.click(scope.getByRole('switch'));
    await waitFor(() => expect(set).toHaveBeenCalledTimes(1));
    expect(set.mock.calls[0][0]).toMatchObject({ enabled: false });
    expect(set.mock.calls[0][0]).not.toHaveProperty('step_up_csrf');
    expect(set.mock.calls[0][0]).not.toHaveProperty('acknowledge_risk');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
  it('requires step-up for a field change without acknowledgement', async () => {
    const scope = await card(); await cap(scope); await stepUp();
    await waitFor(() => expect(set).toHaveBeenCalledWith(expect.objectContaining({ daily_new_accounts: 21, step_up_csrf: TOKEN })));
    expect(set.mock.calls[0][0]).not.toHaveProperty('acknowledge_risk');
  });
  it('requires step-up when disabling with field edits', async () => {
    state.settings.enabled = state.settings.enabled_requested = true;
    const scope = await card();
    await userEvent.clear(scope.getByRole('spinbutton')); await userEvent.type(scope.getByRole('spinbutton'), '25');
    await userEvent.click(scope.getByRole('switch')); await stepUp();
    await waitFor(() => expect(set).toHaveBeenCalledWith(expect.objectContaining({ enabled: false, daily_new_accounts: 25, step_up_csrf: TOKEN })));
  });
  it.each(['direct', 'mfa'])('requires risk acknowledgement for %s', async kind => {
    const scope = await card();
    if (kind === 'direct') await userEvent.click(scope.getByRole('radio', { name: /Create accounts directly/ }));
    else await userEvent.click(scope.getByRole('checkbox', { name: /Allow first-time/ }));
    expect(set).not.toHaveBeenCalled(); await userEvent.click(scope.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText(kind === 'direct'
      ? 'The first Google sign-in within an allowed domain automatically creates an account and AI avatar without your approval, up to the daily limit.'
      : 'When approving a request, you can let staff enroll in two-step verification once within 72 hours.')).toBeInTheDocument();
    await risk(); await stepUp();
    await waitFor(() => expect(set).toHaveBeenCalledWith(expect.objectContaining({ acknowledge_risk: true, step_up_csrf: TOKEN })));
  });
  it('invalidates rejected step-up proof, displays the refusal and asks again on retry', async () => {
    set.mockRejectedValueOnce({ code: 'signin_step_up_required', message: '請先以密碼與驗證器驗證碼確認身分，並在五分鐘內完成操作。' });
    const scope = await card(); await cap(scope); await stepUp();
    expect(await scope.findByRole('alert')).toHaveTextContent('請先以密碼與驗證器驗證碼確認身分，並在五分鐘內完成操作。');
    await userEvent.click(scope.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('dialog')).toBeInTheDocument(); expect(set).toHaveBeenCalledTimes(1);
    await stepUp(); await waitFor(() => expect(set).toHaveBeenCalledTimes(2));
    expect(mockWsClient.call).toHaveBeenCalledTimes(2);
  });
  it.each(['revision', 'missing', 'invalid'])('shows the state-6 sentence for %s template', async kind => {
    state.settings.avatar_template = kind === 'invalid' ? null : 'company:sales'; state.settings.avatar_template_revision = 2;
    if (kind === 'missing') vi.mocked(templateLibrary.catalog).mockResolvedValue({ templates: [] });
    const scope = await card();
    expect(scope.getByText(/has changed since you selected it.*wait for an administrator/)).toBeInTheDocument();
    expect(scope.getByRole('button', { name: 'Select template again' })).toBeInTheDocument();
    expect(scope.queryByText(/Can: /)).not.toBeInTheDocument();
  });
  it('offers only company and blank templates, current revision and safe permission summaries', async () => {
    const scope = await card();
    expect(scope.queryByRole('option', { name: 'Hidden built-in' })).not.toBeInTheDocument();
    await userEvent.selectOptions(scope.getByRole('combobox'), 'company:sales');
    expect(scope.getByText('Sales assistant · Revision 3')).toBeInTheDocument();
    expect(scope.getByText('Can: schedule tasks')).toBeInTheDocument();
    expect(scope.getByText('Cannot: create AI employees')).toBeInTheDocument();
    expect(scope.queryByText('hidden-tool')).not.toBeInTheDocument();
    await userEvent.click(scope.getByRole('button', { name: 'Save' })); await stepUp();
    await waitFor(() => expect(set).toHaveBeenCalledWith(expect.objectContaining({ avatar_template: 'company:sales', avatar_template_revision: 3 })));
  });
  it.each(['changed', 'failed'])('blocks saves when the pre-save read is %s', async kind => {
    const scope = await card();
    if (kind === 'changed') state.settings.approval = 'direct'; else get.mockRejectedValueOnce(new Error('unreachable'));
    await cap(scope); await stepUp();
    await scope.findByRole('alert'); expect(set).not.toHaveBeenCalled();
    if (kind === 'changed') {
      expect(scope.getByRole('alert')).toHaveTextContent('Sign-in settings have changed since you loaded them. Reload before editing.');
      await userEvent.click(scope.getByRole('button', { name: 'Reload' }));
      await waitFor(() => expect(scope.getByRole('radio', { name: /Create accounts directly/ })).toBeChecked());
    }
  });
  it('ignores acknowledgement-only changes and resolves names without exposing ids', async () => {
    state.settings.enabled = state.settings.enabled_requested = true;
    state.settings.ack = { by: 'secret-user-id', at: '2026-11-27T09:40:00Z' };
    const scope = await card(); expect(await scope.findByText(/Last enabled by Alice at/)).toBeInTheDocument();
    expect(scope.queryByText(/secret-user-id/)).not.toBeInTheDocument();
    state.settings.ack.at = '2026-11-28T09:40:00Z';
    await cap(scope); await stepUp(); await waitFor(() => expect(set).toHaveBeenCalledTimes(1));
  });
  it.each(['0', '201', '1.5'])('rejects daily cap %s client-side', async value => {
    const scope = await card(); await cap(scope, value);
    expect(scope.getByRole('alert')).toHaveTextContent('Enter a whole number from 1 to 200 for the daily limit.');
    expect(set).not.toHaveBeenCalled(); expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
  it('reports unknown MFA checks and omits a null redirect URI', async () => {
    state.checks.mfa_policy = 'unknown'; state.redirect_uri = null;
    const scope = await card(); expect(scope.getByText(/Company requires two-step verification.*Unable to confirm/)).toBeInTheDocument();
    expect(scope.queryByRole('button', { name: 'Copy' })).not.toBeInTheDocument();
  });
});
