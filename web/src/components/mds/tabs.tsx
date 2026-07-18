import {
  createContext,
  forwardRef,
  useContext,
  type ComponentPropsWithoutRef,
} from 'react';
import { Tabs as BaseTabs } from '@base-ui/react/tabs';
import { cn } from '@/lib/utils';

/**
 * Tabs — MDS tab strip (spec §4 Tabs), built on @base-ui/react.
 * `variant="default"` = filled pill track; `variant="line"` = underline bar.
 * Compose: Tabs > TabsList > TabsTab + TabsPanel.
 */
type TabsVariant = 'default' | 'line';
const TabsVariantContext = createContext<TabsVariant>('default');

export const Tabs = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseTabs.Root> & { variant?: TabsVariant }
>(({ className, variant = 'default', ...props }, ref) => (
  <TabsVariantContext.Provider value={variant}>
    <BaseTabs.Root
      ref={ref}
      data-slot="tabs"
      data-variant={variant}
      className={cn('flex flex-col gap-2', className)}
      {...props}
    />
  </TabsVariantContext.Provider>
));
Tabs.displayName = 'Tabs';

export const TabsList = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseTabs.List>
>(({ className, ...props }, ref) => {
  const variant = useContext(TabsVariantContext);
  return (
    <BaseTabs.List
      ref={ref}
      data-slot="tabs-list"
      className={cn(
        'inline-flex items-center',
        variant === 'default'
          ? 'h-8 rounded-lg bg-muted p-[3px] text-muted-foreground'
          : 'gap-1 rounded-none bg-transparent',
        className
      )}
      {...props}
    />
  );
});
TabsList.displayName = 'TabsList';

export const TabsTab = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<typeof BaseTabs.Tab>
>(({ className, ...props }, ref) => {
  const variant = useContext(TabsVariantContext);
  return (
    <BaseTabs.Tab
      ref={ref}
      data-slot="tabs-tab"
      className={cn(
        'relative inline-flex items-center justify-center gap-1.5 rounded-md px-1.5 py-0.5 text-sm font-medium text-foreground/60 outline-none transition-colors hover:text-foreground',
        'focus-visible:ring-3 focus-visible:ring-ring/50',
        'data-[disabled]:pointer-events-none data-[disabled]:opacity-50',
        "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4",
        variant === 'default'
          ? 'data-[selected]:bg-background data-[selected]:text-foreground data-[selected]:shadow-sm'
          : "data-[selected]:text-foreground data-[selected]:after:absolute data-[selected]:after:inset-x-0 data-[selected]:after:bottom-[-5px] data-[selected]:after:h-0.5 data-[selected]:after:bg-foreground data-[selected]:after:content-['']",
        className
      )}
      {...props}
    />
  );
});
TabsTab.displayName = 'TabsTab';

export const TabsPanel = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseTabs.Panel>
>(({ className, ...props }, ref) => (
  <BaseTabs.Panel
    ref={ref}
    data-slot="tabs-panel"
    className={cn('outline-none', className)}
    {...props}
  />
));
TabsPanel.displayName = 'TabsPanel';
