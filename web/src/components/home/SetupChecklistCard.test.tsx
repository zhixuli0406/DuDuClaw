import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import { SetupChecklistCard } from './SetupChecklistCard';
import type { SetupData } from '@/lib/setup-checklist';
const state = vi.hoisted(() => ({ allowed: true, data: null as SetupData | null, enabled: vi.fn() }));
vi.mock('@/hooks/useSetupChecklist', () => ({ useSetupChecklist: (enabled: boolean) => { state.enabled(enabled); return state; } }));
vi.mock('@/stores/auth-store', () => ({ useAuthStore: (selector: (s: unknown) => unknown) => selector({ user: { id: 'admin' } }) }));
const complete: SetupData = {
  company: 'Company', knowledge: 1, runtime: true, accounts: 1, google: false, odoo: false,
  agents: 1, users: [{ role: 'admin', status: 'active', bindings: [] }, { role: 'employee', status: 'active', bindings: [{}] }],
  channels: true, skills: 1, routines: 0, tasks: 1,
};
function show() {
  return render(<IntlProvider locale="zh-TW" messages={{ 'setup.title': '設定清單', 'setup.progress': '已完成 {done} / {total}', 'setup.dismiss': '暫時隱藏', 'setup.open': '開啟清單' }}><MemoryRouter><SetupChecklistCard /></MemoryRouter></IntlProvider>);
}
describe('SetupChecklistCard', () => {
  beforeEach(() => { vi.restoreAllMocks(); localStorage.clear(); state.allowed = true; state.data = { ...complete, tasks: 0 }; state.enabled.mockClear(); });
  it('shows an unfinished checklist entry', () => {
    show();
    expect(screen.getByText('已完成 8 / 9')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: '開啟清單' })).toHaveAttribute('href', '/setup');
  });
  it('hides a completed checklist even when optional items are unfinished', () => {
    state.data = complete;
    show();
    expect(screen.queryByText('設定清單')).not.toBeInTheDocument();
  });
  it('persists dismissal and disables subsequent reads', () => {
    show();
    fireEvent.click(screen.getByRole('button', { name: '暫時隱藏' }));
    expect(localStorage.getItem('duduclaw.setup.dismissed.admin')).toBe('1');
    expect(screen.queryByText('設定清單')).not.toBeInTheDocument();
    expect(state.enabled).toHaveBeenLastCalledWith(false);
  });
  it('does not show an already dismissed checklist', () => {
    localStorage.setItem('duduclaw.setup.dismissed.admin', '1');
    show();
    expect(screen.queryByText('設定清單')).not.toBeInTheDocument();
    expect(state.enabled).toHaveBeenLastCalledWith(false);
  });
  it('survives storage read/write failures and dismisses for the current session', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('denied'); });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('denied'); });
    show();
    expect(screen.getByText('設定清單')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '暫時隱藏' }));
    expect(screen.queryByText('設定清單')).not.toBeInTheDocument();
  });
  it('hides from an unauthorized viewer', () => {
    state.allowed = false;
    show();
    expect(screen.queryByText('設定清單')).not.toBeInTheDocument();
  });
});
