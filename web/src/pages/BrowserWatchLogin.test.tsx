import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import '@/test/mocks';
import en from '@/i18n/en.json';
import { AuthGuard } from '@/components/AuthGuard';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { LoginPage } from './LoginPage';

const WATCH = '/computer/alice?l=opaque.signature';
function authenticated() {
  useAuthStore.setState({ isAuthenticated: true });
  useConnectionStore.setState({ state: 'authenticated', everAuthenticated: true });
}
function Destination() {
  const location = useLocation();
  return <p>{`destination:${location.pathname}${location.search}`}</p>;
}
function page(entry: string | { pathname: string; state: unknown }) {
  return <IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[entry]}><Routes>
    <Route path="/login" element={<LoginPage />} />
    <Route element={<AuthGuard />}><Route path="/computer/:agentId" element={<Destination />} /></Route>
    <Route path="/" element={<p>home destination</p>} />
  </Routes></MemoryRouter></IntlProvider>;
}
async function passwordLogin() {
  await userEvent.type(await screen.findByLabelText('Email', { exact: true }), 'fixture@example.invalid');
  await userEvent.type(screen.getByLabelText('Password', { exact: true }), 'fixture-password');
  await userEvent.click(screen.getByRole('button', { name: 'Sign In', exact: true }));
}
beforeEach(() => {
  useAuthStore.setState({
    initialized: true, isAuthenticated: false, loading: false, jwt: null,
    firstRunStatus: vi.fn(async () => false),
    tryLocalSession: vi.fn(async () => false),
    login: vi.fn(async () => { authenticated(); }),
  });
  useConnectionStore.setState({ state: 'disconnected', everAuthenticated: false, mustChangePassword: false });
});

describe('normal sign-in from a browser watch link', () => {
  it('retains the browser destination through the actual auth guard and password form', async () => {
    render(page(WATCH));
    await passwordLogin();
    expect(await screen.findByText(`destination:${WATCH}`)).toBeInTheDocument();
  });
  it('honors a bounded browser destination carried by login route state', async () => {
    render(page({ pathname: '/login', state: { from: WATCH } }));
    await passwordLogin();
    expect(await screen.findByText(`destination:${WATCH}`)).toBeInTheDocument();
  });
  it('preserves the same destination for the existing local-session sign-in', async () => {
    useAuthStore.setState({ tryLocalSession: vi.fn(async () => { authenticated(); return true; }) });
    render(page({ pathname: '/login', state: { from: WATCH } }));
    expect(await screen.findByText(`destination:${WATCH}`)).toBeInTheDocument();
  });
  it.each(['https://outside.invalid/', '//outside.invalid/', '/\\outside.invalid/', '/manage/users', '/computer/alice?l=x&l=y', '/computer/alice?l=opaque.signature#outside'])('refuses a forged or unrelated return target: %s', async (from) => {
    render(page({ pathname: '/login', state: { from } }));
    await passwordLogin();
    expect(await screen.findByText('home destination')).toBeInTheDocument();
  });
});
