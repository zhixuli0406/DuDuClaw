import {
  createContext,
  forwardRef,
  useCallback,
  useContext,
  useRef,
  type ComponentPropsWithoutRef,
  type MouseEvent,
  type ReactNode,
} from 'react';
import { useNavigate } from 'react-router';
import { useVirtualizer } from '@tanstack/react-virtual';
import { ChevronDownIcon, ChevronsUpDownIcon, ChevronUpIcon } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * ListGrid — MDS Linear-style data list (spec §4 ListGrid). A single column
 * template is shared by the header and every row via context so cells align
 * across the whole grid; rows use `@container` queries to drop secondary cells
 * on narrow widths. Optional virtual scrolling (@tanstack/react-virtual).
 *
 * Note (deviation): the spec describes a single container grid + `grid-cols-subgrid`
 * rows with two 0.75rem edge-padding columns. We instead give each row its own
 * grid with the shared template (context) and inset content with `px-3`, keeping
 * hover backgrounds full-bleed. This produces identical column alignment while
 * staying compatible with absolute-positioned virtual rows (subgrid can't inherit
 * tracks from a grid parent once a row is taken out of flow by the virtualizer).
 */

const ListGridColumnsContext = createContext<string>('1fr');

/** Merge a forwarded ref with a local one. */
function useMergedRef<T>(
  forwarded: React.ForwardedRef<T>
): [React.RefObject<T | null>, (node: T | null) => void] {
  const local = useRef<T | null>(null);
  const set = useCallback(
    (node: T | null) => {
      local.current = node;
      if (typeof forwarded === 'function') forwarded(node);
      else if (forwarded) forwarded.current = node;
    },
    [forwarded]
  );
  return [local, set];
}

export interface ListGridVirtualConfig {
  count: number;
  estimateRowHeight?: number;
  overscan?: number;
  renderRow: (index: number) => ReactNode;
}

export const ListGridContainer = forwardRef<
  HTMLDivElement,
  Omit<ComponentPropsWithoutRef<'div'>, 'children'> & {
    columns: string;
    header?: ReactNode;
    children?: ReactNode;
    virtual?: ListGridVirtualConfig;
  }
>(({ className, columns, header, children, virtual, ...props }, ref) => {
  const [scrollRef, setScrollRef] = useMergedRef<HTMLDivElement>(ref);

  return (
    <ListGridColumnsContext.Provider value={columns}>
      <div
        ref={setScrollRef}
        role="table"
        data-slot="list-grid"
        className={cn('@container relative h-full overflow-auto', className)}
        {...props}
      >
        {header}
        {virtual ? (
          <VirtualBody scrollRef={scrollRef} config={virtual} />
        ) : (
          <div role="rowgroup" className="min-w-fit">
            {children}
          </div>
        )}
        {/* bottom clearance so the last row clears floating action bars */}
        <div aria-hidden className="h-16 shrink-0" />
      </div>
    </ListGridColumnsContext.Provider>
  );
});
ListGridContainer.displayName = 'ListGridContainer';

function VirtualBody({
  scrollRef,
  config,
}: {
  scrollRef: React.RefObject<HTMLDivElement | null>;
  config: ListGridVirtualConfig;
}) {
  const { count, estimateRowHeight = 48, overscan = 8, renderRow } = config;
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => estimateRowHeight,
    overscan,
  });
  return (
    <div
      role="rowgroup"
      className="relative min-w-fit"
      style={{ height: virtualizer.getTotalSize() }}
    >
      {virtualizer.getVirtualItems().map((item) => (
        <div
          key={item.key}
          data-index={item.index}
          className="absolute inset-x-0 top-0"
          style={{
            height: item.size,
            transform: `translateY(${item.start}px)`,
          }}
        >
          {renderRow(item.index)}
        </div>
      ))}
    </div>
  );
}

export const ListGridHeader = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'>
>(({ className, style, ...props }, ref) => {
  const columns = useContext(ListGridColumnsContext);
  return (
    <div
      ref={ref}
      role="row"
      data-slot="list-grid-header"
      style={{ gridTemplateColumns: columns, ...style }}
      className={cn(
        'sticky top-0 z-10 grid h-9 min-w-fit items-center gap-x-3 border-b border-surface-border bg-page-canvas px-3',
        className
      )}
      {...props}
    />
  );
});
ListGridHeader.displayName = 'ListGridHeader';

export const ListGridHeaderCell = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'> & {
    sortable?: boolean;
    sortDirection?: 'asc' | 'desc' | null;
    onSort?: () => void;
    hideBelow?: boolean;
  }
>(
  (
    { className, children, sortable, sortDirection = null, onSort, hideBelow, ...props },
    ref
  ) => (
    <div
      ref={ref}
      role="columnheader"
      aria-sort={
        sortable
          ? sortDirection === 'asc'
            ? 'ascending'
            : sortDirection === 'desc'
              ? 'descending'
              : 'none'
          : undefined
      }
      className={cn(
        'flex min-w-0 items-center px-2 text-xs font-medium text-muted-foreground',
        hideBelow && 'hidden @2xl:flex',
        className
      )}
      {...props}
    >
      {sortable ? (
        <button
          type="button"
          onClick={onSort}
          className="inline-flex items-center gap-1 rounded-sm outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          {children}
          {sortDirection === 'asc' ? (
            <ChevronUpIcon className="size-3" />
          ) : sortDirection === 'desc' ? (
            <ChevronDownIcon className="size-3" />
          ) : (
            <ChevronsUpDownIcon className="size-3 opacity-50" />
          )}
        </button>
      ) : (
        children
      )}
    </div>
  )
);
ListGridHeaderCell.displayName = 'ListGridHeaderCell';

/**
 * useRowLink — whole-row navigation that yields to text selection, modifier
 * keys (so ⌘/ctrl-click on the real link in the name cell opens a new tab),
 * and clicks landing on interactive descendants. Returns an onClick handler.
 */
export function useRowLink(to?: string) {
  const navigate = useNavigate();
  return useCallback(
    (e: MouseEvent<HTMLElement>) => {
      if (!to || e.defaultPrevented) return;
      // middle-click or any modifier: let the real <a> handle new-tab intent.
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey)
        return;
      // Don't hijack an active text selection.
      const selection =
        typeof window !== 'undefined' ? window.getSelection?.() : null;
      if (selection && selection.toString().length > 0) return;
      // Ignore clicks that originate from interactive descendants.
      const target = e.target as HTMLElement;
      if (
        target.closest(
          'a,button,input,select,textarea,label,[role="menuitem"],[role="checkbox"],[data-stop-row-nav]'
        )
      )
        return;
      navigate(to);
    },
    [to, navigate]
  );
}

export const ListGridRow = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'> & {
    rowSize?: 'default' | 'lg';
    selected?: boolean;
    /** When set, the whole row navigates here via `useRowLink`. */
    to?: string;
  }
>(
  (
    { className, style, rowSize = 'default', selected, to, onClick, ...props },
    ref
  ) => {
    const columns = useContext(ListGridColumnsContext);
    const rowLink = useRowLink(to);
    return (
      <div
        ref={ref}
        role="row"
        data-slot="list-grid-row"
        data-selected={selected || undefined}
        style={{ gridTemplateColumns: columns, ...style }}
        className={cn(
          'grid min-w-fit cursor-pointer grid-cols-[inherit] items-center gap-x-3 px-3 transition-colors',
          rowSize === 'lg' ? 'min-h-16' : 'h-12',
          'hover:bg-accent/40',
          selected && 'bg-accent/30',
          className
        )}
        onClick={(e) => {
          onClick?.(e);
          if (to) rowLink(e);
        }}
        {...props}
      />
    );
  }
);
ListGridRow.displayName = 'ListGridRow';

export const ListGridCell = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'> & { hideBelow?: boolean }
>(({ className, hideBelow, ...props }, ref) => (
  <div
    ref={ref}
    role="cell"
    data-slot="list-grid-cell"
    className={cn(
      'flex min-w-0 items-center px-2',
      hideBelow && 'hidden @2xl:flex',
      className
    )}
    {...props}
  />
));
ListGridCell.displayName = 'ListGridCell';
