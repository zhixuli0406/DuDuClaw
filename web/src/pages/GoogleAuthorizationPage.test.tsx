import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { IntlProvider } from 'react-intl';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { googleAuthorization } from '@/lib/google-authorization';
import { GoogleAuthorizationPage } from './GoogleAuthorizationPage';
const ticket = 'a'.repeat(64);
beforeEach(() => {
  vi.spyOn(googleAuthorization, 'preview').mockResolvedValue({ agent: 'helper', account: 'alice@example.com',
    purpose: 'base', mode: 'delegate', services: ['https://www.googleapis.com/auth/gmail.readonly'], expires: 123 });
  vi.spyOn(googleAuthorization, 'confirm').mockResolvedValue({ completed: true });
});
afterEach(() => vi.restoreAllMocks());
function page() {
  return render(<IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[`/me/google/authorize#ticket=${ticket}`]}>
    <GoogleAuthorizationPage />
  </MemoryRouter></IntlProvider>);
}
it('renders disclosure on GET and requires both checkboxes before confirmation', async () => {
  const user = userEvent.setup(); page();
  expect(await screen.findByText('Google account: alice@example.com')).toBeInTheDocument();
  const button = screen.getByRole('button', { name: 'Confirm and continue' });
  expect(button).toBeDisabled(); expect(googleAuthorization.confirm).not.toHaveBeenCalled();
  const boxes = screen.getAllByRole('checkbox');
  await user.click(boxes[0]); expect(button).toBeDisabled();
  await user.click(boxes[1]); expect(button).toBeEnabled();
  await user.click(button);
  expect(await screen.findByRole('status')).toHaveTextContent('Access confirmed');
  expect(googleAuthorization.confirm).toHaveBeenCalledTimes(1);
  expect(googleAuthorization.confirm).toHaveBeenCalledWith(ticket);
});
it('does not confirm or expose details when another member opens the link', async () => {
  vi.mocked(googleAuthorization.preview).mockRejectedValue(new Error('refused'));
  page();
  expect(await screen.findByRole('alert')).toHaveTextContent('belongs to another account');
  expect(screen.queryByText('alice@example.com')).not.toBeInTheDocument();
  expect(googleAuthorization.confirm).not.toHaveBeenCalled();
});
