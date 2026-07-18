import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { AppSidebar } from './AppSidebar';
import { useAuthStore } from '@/stores/auth-store';
import { useCommandPaletteStore } from '@/stores/command-palette-store';

function renderSidebar() {
  return renderWithProviders(
    <SidebarProvider>
      <AppSidebar />
    </SidebarProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useAuthStore.setState({
    user: { display_name: 'Boss', role: 'admin' } as never,
    bindings: [],
  } as never);
  useCommandPaletteStore.setState({ open: false });
});

describe('AppSidebar (Multica shell)', () => {
  // Multica IA (spec §5.1): flat daily row + 工作 / 公司 / 設定 groups + a live
  // 員工 zone. Home is the single spine.
  it('renders the flat daily items and the three collapsible group labels', () => {
    renderSidebar();
    // Flat daily items (no group header). Inbox appears twice (the nav row +
    // the footer bell shortcut), so assert at least one.
    expect(screen.getByRole('link', { name: /Home/i })).toBeInTheDocument();
    expect(screen.getAllByRole('link', { name: /Inbox/i }).length).toBeGreaterThan(0);
    expect(screen.getByRole('link', { name: /Chat/i })).toBeInTheDocument();
    // The three group labels.
    expect(screen.getByText(/^Work$/)).toBeInTheDocument();
    expect(screen.getByText(/^Company$/)).toBeInTheDocument();
    expect(screen.getByText(/^Settings$/)).toBeInTheDocument();
    // The primary 交辦 action + the ⌘K search trigger.
    expect(screen.getByRole('button', { name: /New task/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Command palette/i })).toBeInTheDocument();
  });

  it('hides role-gated items below the required role (fail-closed UX)', () => {
    useAuthStore.setState({ user: { display_name: 'E', role: 'employee' } as never });
    renderSidebar();
    // manager+ surfaces are hidden for an employee…
    expect(screen.queryByRole('link', { name: /Reports/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Manage/i })).not.toBeInTheDocument();
    // …while open surfaces stay.
    expect(screen.getByRole('link', { name: /Home/i })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /About/i })).toBeInTheDocument();
  });

  it('marks the current route active (aria-current + accent class)', () => {
    // Default MemoryRouter route is '/', so Home is the active spine.
    renderSidebar();
    const home = screen.getByRole('link', { name: /Home/i });
    expect(home).toHaveAttribute('aria-current', 'page');
    expect(home.className).toContain('bg-sidebar-accent');
    // A non-active item carries neither.
    const chat = screen.getByRole('link', { name: /Chat/i });
    expect(chat).not.toHaveAttribute('aria-current', 'page');
  });

  it('opens the command palette from the search trigger', async () => {
    const user = userEvent.setup();
    renderSidebar();
    expect(useCommandPaletteStore.getState().open).toBe(false);
    await user.click(screen.getByRole('button', { name: /Command palette/i }));
    expect(useCommandPaletteStore.getState().open).toBe(true);
  });
});
