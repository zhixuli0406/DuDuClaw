import { createContext, useCallback, useContext, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { formatError } from '@/lib/toast';
import type { ProtectedConnectorKind } from '@/lib/api';
import { isNewFeature } from '@/apps/registry';
import { useSystemStore } from '@/stores/system-store';
import { Badge } from '@/components/mds';

/** Route of the protected-mode admin page (設定 › 受保護模式). */
export const PROTECTED_MODE_PATH = '/app/system/protected';

/**
 * 「1.81 新功能」 chip of the v1.81 boards. Same rule as the sidebar's 新功能
 * chip (`isNewFeature`): shown while the running version is at or below the
 * release the screen shipped in.
 */
export function NewInBadge({ version }: { version: string }) {
  const intl = useIntl();
  const running = useSystemStore((s) => s.status?.version ?? null);
  if (!isNewFeature(version, running)) return null;
  const short = version.split('.').slice(0, 2).join('.');
  return (
    <Badge className="bg-warning/15 text-warning">
      {intl.formatMessage({ id: 'protectedMode.newIn' }, { version: short })}
    </Badge>
  );
}

/** Small trail above a v1.81 page title: 「設定 › 受保護模式」 + the 新功能 chip. */
export function PageTrail({ trail, newIn }: { trail: string; newIn?: string }) {
  return (
    <p className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
      <span>{trail}</span>
      {newIn && <NewInBadge version={newIn} />}
    </p>
  );
}

/** Every string of this page lives under `protectedMode.*`. */
export function useT() {
  const intl = useIntl();
  return useCallback(
    (key: string, values?: Record<string, string | number>) =>
      intl.formatMessage({ id: `protectedMode.${key}` }, values),
    [intl],
  );
}

/** Optional message: the id when the catalogue has it, otherwise `fallback`. */
export function useOptionalT() {
  const intl = useIntl();
  return useCallback(
    (key: string, fallback: string, values?: Record<string, string | number>) => {
      const id = `protectedMode.${key}`;
      return Object.hasOwn(intl.messages, id) ? intl.formatMessage({ id }, values) : fallback;
    },
    [intl],
  );
}

/**
 * The server's refusal, as the person should read it. The isolation RPCs
 * answer with fixed plain-language sentences (`isolation_*_rpc.rs`), which
 * `formatError` passes through; anything else becomes the generic clause.
 */
export function refusalMessage(error: unknown): string {
  return formatError(error);
}

export interface Person {
  name: string;
  email: string;
}

/** Page-wide lookups shared by the tabs. */
export interface ProtectedPageContext {
  /** users.db id → display name / login email (Admin `users.list`). */
  people: Map<string, Person>;
  /** Switch tabs; `wizard` opens the connector wizard with a preset. */
  goTo: (tab: ProtectedTab, wizard?: WizardPreset) => void;
}

export type ProtectedTab = 'employees' | 'connectors' | 'tenant' | 'audit';
export const PROTECTED_TABS: readonly ProtectedTab[] = ['employees', 'connectors', 'tenant', 'audit'];

export interface WizardPreset {
  employee?: string;
  provider?: string;
  label?: string;
}

const PageContext = createContext<ProtectedPageContext>({ people: new Map(), goTo: () => {} });

export function ProtectedPageProvider({ value, children }: { value: ProtectedPageContext; children: ReactNode }) {
  return <PageContext.Provider value={value}>{children}</PageContext.Provider>;
}

export function useProtectedPage(): ProtectedPageContext {
  return useContext(PageContext);
}

/** A person's name for display; never an internal id. */
export function usePersonName() {
  const { people } = useProtectedPage();
  const t = useT();
  return useCallback(
    (id: string | null | undefined) => {
      if (!id) return t('unknownPerson');
      const person = people.get(id);
      return person?.name || person?.email || t('unknownPerson');
    },
    [people, t],
  );
}

/** Connector services the wizard offers, in display order. */
export const CONNECTOR_PROVIDERS = ['google', 'notion', 'github', 'odoo', 'database', 'computer', 'channel'] as const;
/** Services whose account is a reference to a registered source. */
export const DETAIL_PROVIDERS = ['database', 'computer', 'channel'] as const;

export function useProviderName() {
  const optional = useOptionalT();
  return useCallback(
    (provider: string) => optional(`provider.${provider}`, optional('provider.other', provider)),
    [optional],
  );
}

export function kindKey(kind: ProtectedConnectorKind | string): string {
  return kind === 'personal' || kind === 'shared_tenant' || kind === 'shared_home' ? kind : 'unknown';
}

/** Lower-case agent ids: same shape the gateway accepts for a new employee. */
export const LABEL_PATTERN = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;

export function formatWhen(value: string, locale: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(locale, { dateStyle: 'medium', timeStyle: 'short' });
}

/** A bordered box whose table scrolls sideways on a phone. */
export function TableBox({ children, label }: { children: ReactNode; label: string }) {
  return (
    <div role="region" aria-label={label} tabIndex={0} className="overflow-x-auto rounded-lg border border-surface-border focus-visible:ring-3 focus-visible:ring-ring/50 outline-none">
      {children}
    </div>
  );
}

/** Inline error box for a failed load or a refused action. */
export function Refusal({ message }: { message: string | null }) {
  if (!message) return null;
  return (
    <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
      {message}
    </p>
  );
}
