import { afterEach, expect, it, vi } from 'vitest';
import * as guard from './lib/agent-request-guard';

// No route/consumer is imported: startup itself must register the authorizer.
vi.mock('./App', () => ({ App: () => null }));
vi.mock('react-dom/client', () => ({ createRoot: vi.fn(() => ({ render: vi.fn() })) }));
vi.mock('./components/Toast', () => ({ ToastProvider: () => null }));
vi.mock('./components/layout/AppErrorBoundary', () => ({ AppErrorBoundary: () => null }));
vi.mock('./components/brand/ProductLogo', () => ({ useDarkAppearance: () => false }));
vi.mock('./stores/company-store', () => ({ loadPublicCompany: vi.fn() }));
vi.mock('./stores/brand-store', () => ({ loadPublicBrand: vi.fn(), useBrandStore: vi.fn() }));

afterEach(() => vi.restoreAllMocks());

it('defaults to deny in isolation and registers the authorizer from the app entry', async () => {
  await expect(guard.assertAgentRequestAllowed('agents.pause', { agent_id: 'alice' })).rejects.toThrow('revoked');
  const register = vi.spyOn(guard, 'setAgentRequestAuthorizer');
  await import('./main');
  expect(register).toHaveBeenCalledTimes(1);
  const { api } = await import('./lib/api');
  const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: [], unrestricted: true, default_agent: null });
  await expect(guard.assertAgentRequestAllowed('agents.pause', { agent_id: 'alice' })).resolves.toBeUndefined();
  expect(callable).toHaveBeenCalledTimes(1);
  const { useAuthStore } = await import('./stores/auth-store');
  useAuthStore.setState({ bindings: [] });
});
