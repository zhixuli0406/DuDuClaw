import {
  forwardRef,
  type ComponentPropsWithoutRef,
  type ComponentType,
  type ReactNode,
} from 'react';
import { Tabs as BaseTabs } from '@base-ui/react/tabs';
import { CheckIcon, AlertCircleIcon } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Card } from './card';
import { Spinner } from './spinner';

/**
 * Settings layout primitives (spec §5.3 式3). `SettingsShell` is a controlled
 * two-pane form: a grouped left rail (vertical ≥md, horizontal-scroll on mobile)
 * driving a `max-w-3xl` scrolling content pane. Tab state is owned by the caller
 * (`value`/`onValueChange`) — deliberately router-agnostic so pages sync `?tab=`
 * however they like.
 */

export interface SettingsNavItem {
  value: string;
  label: ReactNode;
  icon?: ComponentType<{ className?: string }>;
}
export interface SettingsNavGroup {
  label?: ReactNode;
  items: SettingsNavItem[];
}

export function SettingsShell({
  value,
  onValueChange,
  groups,
  children,
  className,
}: {
  value: string;
  onValueChange: (value: string) => void;
  groups: SettingsNavGroup[];
  children: ReactNode;
  className?: string;
}) {
  return (
    <BaseTabs.Root
      value={value}
      onValueChange={(next) => onValueChange(String(next))}
      orientation="vertical"
      data-slot="settings-shell"
      className={cn('flex min-h-0 flex-1 flex-col md:flex-row', className)}
    >
      <BaseTabs.List
        data-slot="settings-rail"
        className="flex shrink-0 gap-1 overflow-x-auto border-b border-surface-border p-2 md:w-56 md:flex-col md:overflow-x-visible md:overflow-y-auto md:border-r md:border-b-0 md:p-4"
      >
        {groups.map((group, gi) => (
          <div
            key={gi}
            className="flex shrink-0 gap-1 md:flex md:w-full md:flex-col"
          >
            {group.label && (
              <div className="hidden h-8 items-center px-2 text-xs font-medium text-muted-foreground md:flex">
                {group.label}
              </div>
            )}
            {group.items.map((item) => (
              <BaseTabs.Tab
                key={item.value}
                value={item.value}
                data-slot="settings-rail-item"
                className={cn(
                  'flex h-8 shrink-0 items-center gap-2 rounded-md px-2 text-sm whitespace-nowrap text-muted-foreground outline-none transition-colors',
                  'hover:bg-sidebar-accent/70 focus-visible:ring-2 focus-visible:ring-ring/50',
                  'data-[selected]:bg-sidebar-accent data-[selected]:font-medium data-[selected]:text-sidebar-accent-foreground',
                  "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4"
                )}
              >
                {item.icon && <item.icon />}
                {item.label}
              </BaseTabs.Tab>
            ))}
          </div>
        ))}
      </BaseTabs.List>
      <div
        data-slot="settings-content"
        className="mx-auto w-full max-w-3xl flex-1 overflow-y-auto p-4 md:p-8"
      >
        {children}
      </div>
    </BaseTabs.Root>
  );
}

export const SettingsTab = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseTabs.Panel> & {
    title: ReactNode;
    description?: ReactNode;
  }
>(({ className, title, description, children, ...props }, ref) => (
  <BaseTabs.Panel
    ref={ref}
    data-slot="settings-tab"
    className={cn('space-y-8 outline-none', className)}
    {...props}
  >
    <div className="space-y-1">
      <h2 className="text-xl font-semibold">{title}</h2>
      {description && (
        <p className="text-sm text-muted-foreground">{description}</p>
      )}
    </div>
    {children}
  </BaseTabs.Panel>
));
SettingsTab.displayName = 'SettingsTab';

export function SettingsSection({
  className,
  title,
  description,
  children,
  ...props
}: ComponentPropsWithoutRef<'section'> & {
  title?: ReactNode;
  description?: ReactNode;
}) {
  return (
    <section
      data-slot="settings-section"
      className={cn('space-y-3', className)}
      {...props}
    >
      {(title || description) && (
        <div className="space-y-0.5">
          {title && <h3 className="text-sm font-medium">{title}</h3>}
          {description && (
            <p className="text-xs text-muted-foreground">{description}</p>
          )}
        </div>
      )}
      {children}
    </section>
  );
}

export function SettingsCard({
  className,
  ...props
}: ComponentPropsWithoutRef<typeof Card>) {
  return (
    <Card
      data-slot="settings-card"
      className={cn(
        'gap-0 divide-y divide-surface-border py-0',
        className
      )}
      {...props}
    />
  );
}

const rowTierClass = {
  default: '',
  text: 'sm:w-96',
  'select-wide': 'sm:w-72',
  select: 'sm:w-48',
  code: 'sm:w-40',
} as const;

export type SettingsRowTier = keyof typeof rowTierClass;

export function SettingsRow({
  className,
  label,
  description,
  tier = 'default',
  children,
  ...props
}: Omit<ComponentPropsWithoutRef<'div'>, 'title'> & {
  label: ReactNode;
  description?: ReactNode;
  tier?: SettingsRowTier;
}) {
  return (
    <div
      data-slot="settings-row"
      data-tier={tier}
      className={cn(
        'flex min-h-16 flex-col gap-2 px-4 py-3.5 sm:flex-row sm:items-center sm:justify-between sm:gap-4',
        className
      )}
      {...props}
    >
      <div className="min-w-0 space-y-0.5">
        <div className="text-sm font-medium">{label}</div>
        {description && (
          <div className="text-xs text-muted-foreground">{description}</div>
        )}
      </div>
      <div
        data-slot="settings-row-control"
        className={cn('shrink-0', rowTierClass[tier])}
      >
        {children}
      </div>
    </div>
  );
}

export type SettingsSaveStatus = 'idle' | 'saving' | 'saved' | 'error';

export function SettingsSaveState({
  status,
  savingLabel = 'Saving…',
  savedLabel = 'Saved',
  errorLabel = 'Failed to save',
  className,
}: {
  status: SettingsSaveStatus;
  savingLabel?: ReactNode;
  savedLabel?: ReactNode;
  errorLabel?: ReactNode;
  className?: string;
}) {
  if (status === 'idle') return null;
  return (
    <div
      data-slot="settings-save-state"
      data-status={status}
      role="status"
      className={cn('flex items-center gap-1.5 text-xs', className)}
    >
      {status === 'saving' && (
        <>
          <Spinner className="text-muted-foreground" />
          <span className="text-muted-foreground">{savingLabel}</span>
        </>
      )}
      {status === 'saved' && (
        <>
          <CheckIcon className="size-3.5 text-success" />
          <span className="text-success">{savedLabel}</span>
        </>
      )}
      {status === 'error' && (
        <>
          <AlertCircleIcon className="size-3.5 text-destructive" />
          <span className="text-destructive">{errorLabel}</span>
        </>
      )}
    </div>
  );
}
