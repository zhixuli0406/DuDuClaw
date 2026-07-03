import { create } from 'zustand';

/**
 * Mobile sidebar drawer state. On `md`+ the sidebar is a static column and this
 * flag is ignored; below `md` the sidebar becomes an off-canvas drawer toggled
 * from the Header hamburger and dismissed on navigation / backdrop tap.
 */
interface SidebarStore {
  readonly mobileOpen: boolean;
  openMobile: () => void;
  closeMobile: () => void;
  toggleMobile: () => void;
}

export const useSidebarStore = create<SidebarStore>((set) => ({
  mobileOpen: false,
  openMobile: () => set({ mobileOpen: true }),
  closeMobile: () => set({ mobileOpen: false }),
  toggleMobile: () => set((s) => ({ mobileOpen: !s.mobileOpen })),
}));
