import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { useLocation } from 'react-router';
import { renderWithProviders } from '@/test/render';
import { api } from '@/lib/api';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { FirstLoginCard } from './FirstLoginCard';

const progress = () => ({
  enabled: true, dismissed: false, password_done: true,
  steps: { channel: 'pending', google: 'pending', example: 'pending', capabilities: 'pending' } as const,
  examples: ['Summarize this document.', 'Draft a reply.', 'Remind me tomorrow.'],
});
function Location() { const location = useLocation(); return <output data-testid="location">{JSON.stringify([location.pathname, location.state])}</output>; }
beforeEach(() => {
  vi.restoreAllMocks(); mockWsClient.call.mockReset();
  useAuthStore.setState({ user: { id: 'member', role: 'employee', email: 'member@example.test' } as never });
  vi.spyOn(api.onboarding, 'get').mockResolvedValue(progress());
});
it('persists skipped steps on the server and shows password as already complete', async () => {
  const saved = { ...progress(), steps: { ...progress().steps, channel: 'skipped' as const } };
  const update = vi.spyOn(api.onboarding, 'update').mockResolvedValue(saved);
  renderWithProviders(<FirstLoginCard enabled />);
  expect(await screen.findByText('Set your password')).toBeInTheDocument();
  expect(screen.getByText('Done')).toBeInTheDocument();
  fireEvent.click(screen.getAllByRole('button', { name: 'Skip for now' })[0]);
  await waitFor(() => expect(update).toHaveBeenCalledWith({ step: 'channel', status: 'skipped' }, 'en'));
  expect(await screen.findByText('Skipped')).toBeInTheDocument();
});
it('starts a new chat with a server-provided example without sending it', async () => {
  const fresh = vi.spyOn(useConversationsStore.getState(), 'startNew').mockImplementation(() => {});
  vi.spyOn(api.onboarding, 'update').mockResolvedValue({ ...progress(), steps: { ...progress().steps, example: 'done' } });
  renderWithProviders(<><FirstLoginCard enabled /><Location /></>);
  fireEvent.click(await screen.findByRole('button', { name: 'Try example 2' }));
  await waitFor(() => expect(fresh).toHaveBeenCalledOnce());
  await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent('["/chat",{"draft":"Draft a reply."}]'));
  expect(mockWsClient.call.mock.calls.map(([method]) => method)).not.toContain('chat.send');
});
it('requires opening the help panel and confirming it before marking reading done', async () => {
  const update = vi.spyOn(api.onboarding, 'update').mockResolvedValue({ ...progress(), steps: { ...progress().steps, capabilities: 'done' } });
  renderWithProviders(<FirstLoginCard enabled />);
  fireEvent.click(await screen.findByRole('button', { name: 'Read guide' }));
  expect(screen.getByText('Actions that need approval')).toBeInTheDocument();
  expect(screen.getByText('Privacy and data')).toBeInTheDocument();
  expect(update).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'I have read this' }));
  await waitFor(() => expect(update).toHaveBeenCalledWith({ step: 'capabilities', status: 'done' }, 'en'));
});
it('hides only after the server persists dismissal', async () => {
  let finish!: (value: ReturnType<typeof progress>) => void;
  vi.spyOn(api.onboarding, 'update').mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  renderWithProviders(<FirstLoginCard enabled />);
  fireEvent.click(await screen.findByRole('button', { name: 'Dismiss guide' }));
  expect(screen.getByText('A few steps left')).toBeInTheDocument();
  finish({ ...progress(), dismissed: true });
  await waitFor(() => expect(screen.queryByText('A few steps left')).not.toBeInTheDocument());
});
it('does not show a completed guide or fetch when disabled', async () => {
  const get = vi.mocked(api.onboarding.get).mockResolvedValue({ ...progress(), steps: { channel: 'skipped', google: 'done', example: 'done', capabilities: 'done' } });
  const view = renderWithProviders(<FirstLoginCard enabled />);
  await waitFor(() => expect(get).toHaveBeenCalledOnce());
  expect(screen.queryByText('A few steps left')).not.toBeInTheDocument();
  view.unmount(); get.mockClear();
  renderWithProviders(<FirstLoginCard enabled={false} />);
  expect(get).not.toHaveBeenCalled();
});
it('does not send an employee to restricted member management for channel setup', async () => {
  renderWithProviders(<FirstLoginCard enabled />);
  fireEvent.click((await screen.findAllByRole('button', { name: 'Set up' }))[0]);
  expect(screen.getByText(/Ask your administrator to verify/)).toBeInTheDocument();
  expect(screen.queryByRole('link', { name: 'Open member management' })).not.toBeInTheDocument();
});
it('leaves the guide visible when saving progress fails', async () => {
  vi.spyOn(api.onboarding, 'update').mockRejectedValue(new Error('save failed'));
  renderWithProviders(<FirstLoginCard enabled />);
  fireEvent.click(await screen.findByRole('button', { name: 'Dismiss guide' }));
  await waitFor(() => expect(screen.getByRole('button', { name: 'Dismiss guide' })).not.toBeDisabled());
  expect(screen.getByText('A few steps left')).toBeInTheDocument();
});

const signinSteps = (): import('@/lib/api').SigninSteps => ({
  mfa: { status: 'done' }, avatar: { status: 'preparing' }, google: { status: 'pending' },
  google_chat: { status: 'admin_binds' }, odoo: { status: 'pending' },
});
function signinProgress(steps = signinSteps()) {
  return { ...progress(), signin_steps: steps, steps_remaining: 99,
    steps: { channel: 'done', google: 'done', example: 'done', capabilities: 'done' } as const };
}
it('shows five live sign-in steps in order and computes 1／5 independently of legacy progress and remaining count', async () => {
  vi.mocked(api.onboarding.get).mockResolvedValue(signinProgress());
  renderWithProviders(<FirstLoginCard enabled />);
  expect(await screen.findByText('1／5')).toBeInTheDocument();
  const rows = screen.getAllByRole('listitem');
  expect(rows).toHaveLength(5);
  ['Two-step verification', 'AI avatar', 'Link Google', 'Google Chat', 'Odoo'].forEach((name, index) => expect(rows[index]).toHaveTextContent(name));
  expect(screen.getByText('Configured')).toBeInTheDocument();
  expect(screen.getByText('Preparing')).toBeInTheDocument();
  expect(screen.getByText('Waiting for administrator')).toBeInTheDocument();
  expect(screen.getByRole('link', { name: 'Link Google' })).toHaveAttribute('href', '/me/connections');
  expect(screen.getByText('Use the email you signed in with: member@example.test.')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Skip for now' })).not.toBeInTheDocument();
  expect(screen.queryByText('Set your password')).not.toBeInTheDocument();
});
it('excludes administrator-managed Odoo from the denominator and completed count', async () => {
  vi.mocked(api.onboarding.get).mockResolvedValue(signinProgress({ ...signinSteps(), odoo: { status: 'not_applicable' } }));
  renderWithProviders(<FirstLoginCard enabled />);
  expect(await screen.findByText('1／4')).toBeInTheDocument();
  expect(screen.getByText('Set up by your administrator')).toBeInTheDocument();
  expect(screen.queryByRole('link', { name: 'Open My Connections' })).not.toBeInTheDocument();
});
it.each([
  ['mfa', 'recommended', 'Set up two-step verification to protect your account.'],
  ['mfa', 'required', 'Set up two-step verification to protect your account.'],
  ['mfa', 'unknown', 'Unable to confirm right now'],
  ['avatar', 'done', 'Done'],
  ['avatar', 'preparing', 'Your AI avatar is being prepared. This usually takes a few minutes.'],
  ['avatar', 'waiting_admin', 'Your administrator is creating your AI avatar. This step will be checked automatically when it is ready.'],
  ['google', 'done', 'Done'],
  ['google', 'blocked_domain', 'Your email domain cannot link to Google right now. Ask your administrator to update the allowed domains in Google integration settings.'],
  ['google_chat', 'done', 'Done'],
  ['google_chat', 'admin_binds', 'Ask your administrator to link your Google Chat on the members page.'],
  ['odoo', 'done', 'Done'],
  ['odoo', 'pending', 'Enter your Odoo account in My Connections.'],
  ['odoo', 'needs_reverify', 'Your Odoo account needs to be verified again. Go to My Connections.'],
  ['odoo', 'invalid', 'Your Odoo account needs to be verified again. Go to My Connections.'],
  ['odoo', 'unknown', 'Unable to confirm right now'],
] as const)('renders %s %s status text', async (step, status, text) => {
  vi.mocked(api.onboarding.get).mockResolvedValue(signinProgress({ ...signinSteps(), [step]: { status } }));
  renderWithProviders(<FirstLoginCard enabled />);
  expect(await screen.findByText(text)).toBeInTheDocument();
  if (step === 'mfa' && (status === 'recommended' || status === 'required')) {
    expect(screen.getByRole('link', { name: 'Open security settings' })).toHaveAttribute('href', '/me/account?tab=account');
    expect(screen.getByText('0／5')).toBeInTheDocument();
  }
  if (step === 'odoo' && ['pending', 'needs_reverify', 'invalid'].includes(status)) {
    expect(screen.getByRole('link', { name: 'Open My Connections' })).toHaveAttribute('href', '/me/connections');
  }
});
it('counts all completed live steps and preserves dismissal without letting members mark a step done', async () => {
  const saved = signinProgress(Object.fromEntries(Object.keys(signinSteps()).map(key => [key, { status: 'done' }])) as import('@/lib/api').SigninSteps);
  vi.mocked(api.onboarding.get).mockResolvedValue(saved);
  const update = vi.spyOn(api.onboarding, 'update').mockResolvedValue({ ...saved, dismissed: true });
  renderWithProviders(<FirstLoginCard enabled />);
  expect(await screen.findByText('5／5')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Dismiss guide' }));
  await waitFor(() => expect(screen.queryByText('5／5')).not.toBeInTheDocument());
  expect(update).toHaveBeenCalledWith({ dismissed: true }, 'en');
});
