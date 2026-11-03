import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import { SetupChecklistPage } from './SetupChecklistPage';
import type { SetupData } from '@/lib/setup-checklist';
const state = vi.hoisted(() => ({ allowed: true, data: null as SetupData | null }));
vi.mock('@/hooks/useSetupChecklist', () => ({ useSetupChecklist: () => state }));
vi.mock('@/components/mds', () => ({
  PageHeader: ({ children }: { children: React.ReactNode }) => <header>{children}</header>,
  Card: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
  CardHeader: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  CardTitle: ({ children }: { children: React.ReactNode }) => <h2>{children}</h2>,
  CardContent: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Badge: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}));
const messages = {
  'setup.title': '設定清單', 'setup.progress': '已完成 {done} / {total}',
  'setup.unknown': '無法確認', 'setup.loading': '讀取中', 'setup.optional': '（選填）',
  'setup.section.company': '公司', 'setup.section.people': '人員',
  'setup.section.capabilities': '能力', 'setup.section.start': '開始',
};
function show() {
  return render(<IntlProvider locale="zh-TW" messages={messages} onError={() => {}}><MemoryRouter><SetupChecklistPage /></MemoryRouter></IntlProvider>);
}
describe('SetupChecklistPage', () => {
  it('shows failed reads as 無法確認 without crediting progress', () => {
    state.allowed = true;
    state.data = { company: null, knowledge: null, runtime: null, accounts: null, google: null, odoo: null, agents: null, users: null, channels: null, skills: null, routines: null, tasks: null };
    show();
    expect(screen.getAllByText('無法確認')).toHaveLength(12);
    expect(screen.getByText('已完成 0 / 8')).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: '公司' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: '人員' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: '能力' })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: '開始' })).toBeInTheDocument();
  });
  it('does not render checklist for an unauthorized viewer', () => {
    state.allowed = false;
    show();
    expect(screen.queryByText('設定清單')).not.toBeInTheDocument();
  });
});
