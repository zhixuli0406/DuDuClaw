import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import { PanelRightClose, PanelRightOpen, X } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * PropertiesPanel — the right-hand context column of the three-pane shell
 * (paperclip P1). Pages inject content via `usePanel().setPanel(...)`; the
 * panel renders it as a fixed 320px column on desktop (collapsible, preference
 * remembered) and as a bottom sheet on mobile.
 *
 * V0 delivers the primitive + context; the shell (V1) mounts `PanelProvider`
 * high in the tree and places `<PropertiesPanel />` in the layout row.
 */

const COLLAPSE_KEY = 'duduclaw:ui:panel-collapsed';

interface PanelState {
  title: ReactNode | null;
  content: ReactNode | null;
  collapsed: boolean;
  sheetOpen: boolean;
  setPanel: (opts: { title?: ReactNode; content: ReactNode }) => void;
  clearPanel: () => void;
  setCollapsed: (v: boolean) => void;
  toggleCollapsed: () => void;
  setSheetOpen: (v: boolean) => void;
}

const noop = () => {};
const DEFAULT: PanelState = {
  title: null,
  content: null,
  collapsed: false,
  sheetOpen: false,
  setPanel: noop,
  clearPanel: noop,
  setCollapsed: noop,
  toggleCollapsed: noop,
  setSheetOpen: noop,
};

const PanelContext = createContext<PanelState>(DEFAULT);

/** Access the properties-panel controller. Safe (no-op) outside a provider. */
export function usePanel(): PanelState {
  return useContext(PanelContext);
}

function readCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSE_KEY) === '1';
  } catch {
    return false;
  }
}

export function PanelProvider({ children }: { children: ReactNode }) {
  const [title, setTitle] = useState<ReactNode | null>(null);
  const [content, setContent] = useState<ReactNode | null>(null);
  const [collapsed, setCollapsedState] = useState<boolean>(readCollapsed);
  const [sheetOpen, setSheetOpen] = useState(false);

  const setCollapsed = useCallback((v: boolean) => {
    setCollapsedState(v);
    try {
      localStorage.setItem(COLLAPSE_KEY, v ? '1' : '0');
    } catch {
      /* private mode — preference just won't persist */
    }
  }, []);

  const setPanel = useCallback<PanelState['setPanel']>((opts) => {
    setTitle(opts.title ?? null);
    setContent(opts.content);
  }, []);

  const clearPanel = useCallback(() => {
    setTitle(null);
    setContent(null);
    setSheetOpen(false);
  }, []);

  const value = useMemo<PanelState>(
    () => ({
      title,
      content,
      collapsed,
      sheetOpen,
      setPanel,
      clearPanel,
      setCollapsed,
      toggleCollapsed: () => setCollapsed(!collapsed),
      setSheetOpen,
    }),
    [title, content, collapsed, sheetOpen, setPanel, clearPanel, setCollapsed],
  );

  return <PanelContext.Provider value={value}>{children}</PanelContext.Provider>;
}

/**
 * The visual shell. Desktop: a collapsible 320px right column. Mobile: a
 * bottom sheet that slides up when `sheetOpen` and there is content.
 */
export function PropertiesPanel({ className }: { className?: string }) {
  const { title, content, collapsed, toggleCollapsed, sheetOpen, setSheetOpen } = usePanel();

  const header = (onClose: () => void, closeIcon: ReactNode) => (
    <div className="flex items-center justify-between gap-2 border-b border-surface-border px-3 py-2">
      <span className="truncate text-sm font-semibold text-foreground">
        {title}
      </span>
      <button
        type="button"
        onClick={onClose}
        className="grid h-7 w-7 place-items-center rounded-lg text-muted-foreground outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
        aria-label="Collapse panel"
      >
        {closeIcon}
      </button>
    </div>
  );

  return (
    <>
      {/* Desktop right column */}
      <aside
        className={cn(
          'hidden shrink-0 border-l border-surface-border bg-surface md:flex md:flex-col',
          collapsed ? 'w-10' : 'w-80',
          'transition-[width] duration-200',
          className,
        )}
        aria-label="Properties"
      >
        {collapsed ? (
          <button
            type="button"
            onClick={toggleCollapsed}
            className="grid h-10 w-10 place-items-center text-muted-foreground outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50"
            aria-label="Expand panel"
          >
            <PanelRightOpen className="h-4 w-4" />
          </button>
        ) : (
          <>
            {header(toggleCollapsed, <PanelRightClose className="h-4 w-4" />)}
            <div className="min-h-0 flex-1 overflow-y-auto p-3">
              {content ?? (
                <p className="px-1 pt-2 text-xs text-muted-foreground">—</p>
              )}
            </div>
          </>
        )}
      </aside>

      {/* Mobile bottom sheet */}
      {sheetOpen && content && (
        <div className="fixed inset-0 z-[120] md:hidden">
          <button
            type="button"
            aria-label="Close"
            onClick={() => setSheetOpen(false)}
            className="absolute inset-0 cursor-default bg-black/30 dark:bg-black/50"
          />
          <div className="absolute inset-x-0 bottom-0 max-h-[80vh] overflow-y-auto rounded-t-3xl border-t border-surface-border bg-surface pb-[env(safe-area-inset-bottom)]">
            {header(() => setSheetOpen(false), <X className="h-4 w-4" />)}
            <div className="p-3">{content}</div>
          </div>
        </div>
      )}
    </>
  );
}
