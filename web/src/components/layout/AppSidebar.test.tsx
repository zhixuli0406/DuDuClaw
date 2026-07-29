import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { AppSidebar } from './AppSidebar';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
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
  // Default to the non-personal layout; the personal-IA test opts in itself.
  useSystemStore.setState({ status: null } as never);
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

  // Personal IA (2026-07-29 client feedback): flat 主區 (daily + Routines /
  // World / Skills / Knowledge), no 工作/公司 groups, no staff zone; 設定 and
  // 進階 close the rail collapsed. Roster/org/pet-studio hidden (pet studio is
  // desktop-only and jsdom is not Tauri).
  it('personal edition: minimal primary rail, collapsed 設定/進階, staff surfaces hidden', async () => {
    const user = userEvent.setup();
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderSidebar();

    // 主區 flat items are present.
    expect(screen.getByRole('link', { name: /Routines/i })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Knowledge/i })).toBeInTheDocument();
    // Enterprise group labels are gone; 進階 exists but starts collapsed.
    expect(screen.queryByText(/^Work$/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^Company$/)).not.toBeInTheDocument();
    expect(screen.getByText(/^Advanced$/)).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Task Board/i })).not.toBeInTheDocument();
    // 設定 collapsed by default too.
    expect(screen.queryByRole('link', { name: /About/i })).not.toBeInTheDocument();
    // Hidden on Personal: AI staff roster + pet studio (desktop-only).
    expect(screen.queryByRole('link', { name: /^Agents$/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Pet studio/i })).not.toBeInTheDocument();

    // Expanding 進階 reveals the folded work surfaces.
    await user.click(screen.getByText(/^Advanced$/));
    expect(screen.getByRole('link', { name: /Task Board/i })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Memory/i })).toBeInTheDocument();
  });

  it('opens the command palette from the search trigger', async () => {
    const user = userEvent.setup();
    renderSidebar();
    expect(useCommandPaletteStore.getState().open).toBe(false);
    await user.click(screen.getByRole('button', { name: /Command palette/i }));
    expect(useCommandPaletteStore.getState().open).toBe(true);
  });
});
