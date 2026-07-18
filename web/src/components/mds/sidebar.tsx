import {
  createContext,
  forwardRef,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type CSSProperties,
} from 'react';
import { cva, type VariantProps } from 'class-variance-authority';
import { PanelLeftIcon } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from './button';
import { Sheet, SheetContent } from './sheet';

/**
 * Sidebar — MDS app-shell navigation rail (spec §5.1).
 *
 * `SidebarProvider` owns the open/collapsed state (persisted to localStorage),
 * the drag-resizable width (`sidebar_width`, clamped 200–360px), and the mobile
 * breakpoint. `Sidebar variant="inset"` renders a floating island on desktop and
 * a Sheet drawer (18rem) on mobile; `SidebarInset` is the page-canvas surface.
 */

const SIDEBAR_WIDTH_DEFAULT = 256;
const SIDEBAR_WIDTH_MIN = 200;
const SIDEBAR_WIDTH_MAX = 360;
const SIDEBAR_WIDTH_ICON = '3rem';
const SIDEBAR_WIDTH_MOBILE = '18rem';
const STORAGE_OPEN = 'sidebar_open';
const STORAGE_WIDTH = 'sidebar_width';
const MOBILE_BREAKPOINT = 768;

function readStorage(key: string): string | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage.getItem(key) : null;
  } catch {
    return null;
  }
}
function writeStorage(key: string, value: string): void {
  try {
    localStorage?.setItem(key, value);
  } catch {
    /* ignore quota / disabled storage */
  }
}

export function useIsMobile(breakpoint = MOBILE_BREAKPOINT): boolean {
  const query = `(max-width: ${breakpoint - 1}px)`;
  const [isMobile, setIsMobile] = useState<boolean>(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function')
      return false;
    return window.matchMedia(query).matches;
  });
  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function')
      return;
    const mql = window.matchMedia(query);
    const onChange = () => setIsMobile(mql.matches);
    onChange();
    mql.addEventListener?.('change', onChange);
    return () => mql.removeEventListener?.('change', onChange);
  }, [query]);
  return isMobile;
}

type SidebarState = 'expanded' | 'collapsed';

interface SidebarContextValue {
  state: SidebarState;
  open: boolean;
  setOpen: (open: boolean) => void;
  toggleSidebar: () => void;
  isMobile: boolean;
  openMobile: boolean;
  setOpenMobile: (open: boolean) => void;
  width: number;
  setWidth: (width: number) => void;
}

const SidebarContext = createContext<SidebarContextValue | null>(null);

export function useSidebar(): SidebarContextValue {
  const ctx = useContext(SidebarContext);
  if (!ctx) throw new Error('useSidebar must be used within a <SidebarProvider>');
  return ctx;
}

function clampWidth(px: number): number {
  return Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, Math.round(px)));
}

export const SidebarProvider = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'> & { defaultOpen?: boolean }
>(({ className, style, children, defaultOpen = true, ...props }, ref) => {
  const isMobile = useIsMobile();
  const [openMobile, setOpenMobile] = useState(false);
  const [open, setOpenState] = useState<boolean>(() => {
    const stored = readStorage(STORAGE_OPEN);
    return stored === null ? defaultOpen : stored === '1';
  });
  const [width, setWidthState] = useState<number>(() => {
    const stored = Number(readStorage(STORAGE_WIDTH));
    return stored ? clampWidth(stored) : SIDEBAR_WIDTH_DEFAULT;
  });

  const setOpen = useCallback((next: boolean) => {
    setOpenState(next);
    writeStorage(STORAGE_OPEN, next ? '1' : '0');
  }, []);

  const setWidth = useCallback((next: number) => {
    const clamped = clampWidth(next);
    setWidthState(clamped);
    writeStorage(STORAGE_WIDTH, String(clamped));
  }, []);

  const toggleSidebar = useCallback(() => {
    if (isMobile) setOpenMobile((v) => !v);
    else setOpen(!open);
  }, [isMobile, open, setOpen]);

  const value = useMemo<SidebarContextValue>(
    () => ({
      state: open ? 'expanded' : 'collapsed',
      open,
      setOpen,
      toggleSidebar,
      isMobile,
      openMobile,
      setOpenMobile,
      width,
      setWidth,
    }),
    [open, setOpen, toggleSidebar, isMobile, openMobile, width, setWidth]
  );

  return (
    <SidebarContext.Provider value={value}>
      <div
        ref={ref}
        data-slot="sidebar-provider"
        style={
          {
            '--sidebar-width': `${width}px`,
            '--sidebar-width-icon': SIDEBAR_WIDTH_ICON,
            ...style,
          } as CSSProperties
        }
        className={cn('flex h-svh w-full bg-app-shell', className)}
        {...props}
      >
        {children}
      </div>
    </SidebarContext.Provider>
  );
});
SidebarProvider.displayName = 'SidebarProvider';

export const Sidebar = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'> & { variant?: 'inset' | 'sidebar' }
>(({ className, children, variant = 'inset', ...props }, ref) => {
  const { state, isMobile, openMobile, setOpenMobile, width } = useSidebar();

  if (isMobile) {
    return (
      <Sheet open={openMobile} onOpenChange={setOpenMobile}>
        <SheetContent
          side="left"
          data-slot="sidebar"
          data-mobile="true"
          className="w-[--sidebar-width-mobile] max-w-none gap-0 bg-sidebar p-0 text-sidebar-foreground [--sidebar-width-mobile:18rem]"
          style={{ ['--sidebar-width-mobile' as string]: SIDEBAR_WIDTH_MOBILE }}
          showClose={false}
        >
          <div className="flex h-full w-full flex-col">{children}</div>
        </SheetContent>
      </Sheet>
    );
  }

  const collapsed = state === 'collapsed';
  return (
    <aside
      ref={ref}
      data-slot="sidebar"
      data-variant={variant}
      data-state={state}
      style={{ width: collapsed ? SIDEBAR_WIDTH_ICON : `${width}px` }}
      className={cn(
        'relative hidden shrink-0 transition-[width] duration-200 ease-out md:block',
        variant === 'inset' && 'p-2',
        className
      )}
      {...props}
    >
      <div className="flex h-full w-full flex-col overflow-hidden bg-sidebar text-sidebar-foreground">
        {children}
      </div>
    </aside>
  );
});
Sidebar.displayName = 'Sidebar';

export const SidebarRail = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<'button'>
>(({ className, ...props }, ref) => {
  const { width, setWidth, isMobile } = useSidebar();
  const dragging = useRef(false);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLButtonElement>) => {
      e.preventDefault();
      dragging.current = true;
      const startX = e.clientX;
      const startWidth = width;
      const onMove = (ev: PointerEvent) => {
        if (!dragging.current) return;
        setWidth(startWidth + (ev.clientX - startX));
      };
      const onUp = () => {
        dragging.current = false;
        window.removeEventListener('pointermove', onMove);
        window.removeEventListener('pointerup', onUp);
      };
      window.addEventListener('pointermove', onMove);
      window.addEventListener('pointerup', onUp);
    },
    [width, setWidth]
  );

  if (isMobile) return null;
  return (
    <button
      ref={ref}
      type="button"
      data-slot="sidebar-rail"
      aria-label="Resize sidebar"
      tabIndex={-1}
      onPointerDown={onPointerDown}
      className={cn(
        'absolute inset-y-0 right-0 z-20 hidden w-4 cursor-ew-resize touch-none select-none md:block',
        'after:absolute after:inset-y-0 after:right-2 after:w-px after:bg-transparent hover:after:bg-sidebar-border',
        className
      )}
      {...props}
    />
  );
});
SidebarRail.displayName = 'SidebarRail';

export function SidebarHeader({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sidebar-header"
      className={cn('flex flex-col gap-2 px-2 py-3', className)}
      {...props}
    />
  );
}

export function SidebarContent({
  className,
  style,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sidebar-content"
      style={{
        maskImage:
          'linear-gradient(to bottom, transparent 0, #000 12px, #000 calc(100% - 12px), transparent 100%)',
        WebkitMaskImage:
          'linear-gradient(to bottom, transparent 0, #000 12px, #000 calc(100% - 12px), transparent 100%)',
        ...style,
      }}
      className={cn(
        'flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto px-2',
        className
      )}
      {...props}
    />
  );
}

export function SidebarFooter({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sidebar-footer"
      className={cn('flex flex-col gap-2 p-2', className)}
      {...props}
    />
  );
}

export function SidebarGroup({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sidebar-group"
      className={cn('relative flex w-full flex-col', className)}
      {...props}
    />
  );
}

export function SidebarGroupLabel({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sidebar-group-label"
      className={cn(
        'flex h-8 shrink-0 items-center px-2 text-xs font-medium text-sidebar-foreground/70',
        className
      )}
      {...props}
    />
  );
}

export const SidebarMenu = forwardRef<
  HTMLUListElement,
  ComponentPropsWithoutRef<'ul'>
>(({ className, ...props }, ref) => (
  <ul
    ref={ref}
    data-slot="sidebar-menu"
    className={cn('flex w-full flex-col gap-0.5', className)}
    {...props}
  />
));
SidebarMenu.displayName = 'SidebarMenu';

export const SidebarMenuItem = forwardRef<
  HTMLLIElement,
  ComponentPropsWithoutRef<'li'>
>(({ className, ...props }, ref) => (
  <li
    ref={ref}
    data-slot="sidebar-menu-item"
    className={cn('relative', className)}
    {...props}
  />
));
SidebarMenuItem.displayName = 'SidebarMenuItem';

export const sidebarMenuButtonVariants = cva(
  'flex w-full items-center gap-2 overflow-hidden rounded-md text-left text-sm outline-none transition-colors ' +
    'text-muted-foreground hover:bg-sidebar-accent/70 ' +
    'focus-visible:ring-2 focus-visible:ring-sidebar-ring ' +
    'data-[active=true]:bg-sidebar-accent data-[active=true]:font-medium data-[active=true]:text-sidebar-accent-foreground ' +
    "[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
  {
    variants: {
      size: {
        default: 'h-8 p-2',
        sm: 'h-7 p-1.5 text-xs',
        lg: 'h-10 p-2.5',
      },
    },
    defaultVariants: { size: 'default' },
  }
);

export const SidebarMenuButton = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<'button'> &
    VariantProps<typeof sidebarMenuButtonVariants> & { isActive?: boolean }
>(({ className, size, isActive = false, type = 'button', ...props }, ref) => (
  <button
    ref={ref}
    type={type}
    data-slot="sidebar-menu-button"
    data-active={isActive}
    className={cn(sidebarMenuButtonVariants({ size }), className)}
    {...props}
  />
));
SidebarMenuButton.displayName = 'SidebarMenuButton';

export function SidebarMenuBadge({
  className,
  ...props
}: ComponentPropsWithoutRef<'span'>) {
  return (
    <span
      data-slot="sidebar-menu-badge"
      className={cn(
        'ml-auto flex h-5 min-w-5 items-center justify-center text-xs tabular-nums text-muted-foreground',
        className
      )}
      {...props}
    />
  );
}

export const SidebarTrigger = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<typeof Button>
>(({ className, onClick, ...props }, ref) => {
  const { toggleSidebar } = useSidebar();
  return (
    <Button
      ref={ref}
      variant="ghost"
      size="icon-sm"
      data-slot="sidebar-trigger"
      aria-label="Toggle sidebar"
      className={cn(className)}
      onClick={(e) => {
        onClick?.(e);
        toggleSidebar();
      }}
      {...props}
    >
      <PanelLeftIcon />
    </Button>
  );
});
SidebarTrigger.displayName = 'SidebarTrigger';

export function SidebarInset({
  className,
  ...props
}: ComponentPropsWithoutRef<'main'>) {
  return (
    <main
      data-slot="sidebar-inset"
      className={cn(
        'relative m-2 flex flex-1 flex-col overflow-hidden rounded-xl bg-page-canvas shadow-[var(--surface-shadow)] ring-1 ring-surface-border',
        className
      )}
      {...props}
    />
  );
}
