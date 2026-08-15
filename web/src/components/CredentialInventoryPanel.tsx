import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { KeyRound, Lock, FileText, Cloud, Terminal, HelpCircle, AlertTriangle } from 'lucide-react';
import { api, type CredentialEntry, type CredentialSourceKind } from '@/lib/api';
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Badge,
  Button,
  ErrorState,
  Skeleton,
} from '@/components/mds';

/**
 * CredentialInventoryPanel (WP-H1 P1) — the structured credential list.
 *
 * `CredentialHygienePanel` answers "what is wrong". This answers the prior
 * question: *what credentials does this deployment have, and where does each
 * one actually come from* — the design doc's `describe()` contract
 * (commercial/docs/DESIGN-credentials-doctrine-2026-08.md §2.3) rendered for a
 * whole config file. Before it, "which of my tokens is a `secret://` reference
 * and which is still plaintext" was answerable only by hand-reading
 * `config.toml`.
 *
 * Two properties are load-bearing and both come from the backend:
 *
 *  - **No values.** `describe()` never holds one, so unlike the old
 *    `system.config` render there is nothing here to mask and nothing that can
 *    leak by forgetting to.
 *  - **No backend calls.** `describe()` classifies the reference without
 *    resolving it, so listing forty fields costs zero Vault round-trips.
 *
 * A field's row therefore reports where its value *would* come from, not
 * whether that source currently answers — resolution failures surface in
 * `duduclaw doctor` and the gateway log, deliberately not here.
 */
export function CredentialInventoryPanel() {
  const intl = useIntl();
  const [loading, setLoading] = useState(true);
  const [entries, setEntries] = useState<CredentialEntry[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [showUnset, setShowUnset] = useState(false);

  const load = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    api.security
      .credentialInventory()
      .then((res) => setEntries(res.entries))
      .catch((e) => {
        setLoadError(e);
        setEntries(null);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const all = useMemo(() => entries ?? [], [entries]);
  const visible = useMemo(
    () => (showUnset ? all : all.filter((e) => e.configured || e.residue)),
    [all, showUnset],
  );
  const counts = useMemo(
    () => ({
      referenced: all.filter((e) => isExternalReference(e.source)).length,
      plaintext: all.filter((e) => e.source === 'legacy').length,
      residue: all.filter((e) => e.residue).length,
      unset: all.filter((e) => !e.configured && !e.residue).length,
    }),
    [all],
  );

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <KeyRound className="size-4 text-brand" />
          {intl.formatMessage({ id: 'security.credentialInventory.title' })}
        </CardTitle>
        <CardDescription>
          {intl.formatMessage({ id: 'security.credentialInventory.desc' })}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {loadError != null ? (
          <ErrorState
            variant="inline"
            error={loadError}
            title={intl.formatMessage({ id: 'security.credentialInventory.loadErrorTitle' })}
            onRetry={load}
          />
        ) : loading ? (
          <div className="space-y-2">
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-9 w-full" />
          </div>
        ) : all.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'security.credentialInventory.empty' })}
          </p>
        ) : (
          <div className="space-y-4">
            <div className="flex flex-wrap gap-2 text-xs">
              <Badge variant="secondary">
                {intl.formatMessage(
                  { id: 'security.credentialInventory.summary.total' },
                  { count: all.length },
                )}
              </Badge>
              <Badge variant="secondary" className="bg-success/15 text-success">
                {intl.formatMessage(
                  { id: 'security.credentialInventory.summary.referenced' },
                  { count: counts.referenced },
                )}
              </Badge>
              {counts.plaintext > 0 && (
                <Badge variant="secondary" className="bg-warning/15 text-warning">
                  {intl.formatMessage(
                    { id: 'security.credentialInventory.summary.plaintext' },
                    { count: counts.plaintext },
                  )}
                </Badge>
              )}
              {counts.residue > 0 && (
                <Badge variant="secondary" className="bg-destructive/10 text-destructive">
                  {intl.formatMessage(
                    { id: 'security.credentialInventory.summary.residue' },
                    { count: counts.residue },
                  )}
                </Badge>
              )}
            </div>

            <div className="space-y-1.5">
              {visible.map((e) => (
                <InventoryRow key={e.path} entry={e} />
              ))}
            </div>

            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'security.credentialInventory.referenceHint' })}
            </p>
          </div>
        )}

        <div className="mt-4 flex flex-wrap items-center justify-end gap-2 border-t border-surface-border pt-3">
          {counts.unset > 0 && (
            <Button variant="ghost" size="sm" onClick={() => setShowUnset((v) => !v)}>
              {intl.formatMessage(
                {
                  id: showUnset
                    ? 'security.credentialInventory.hideUnset'
                    : 'security.credentialInventory.showUnset',
                },
                { count: counts.unset },
              )}
            </Button>
          )}
          <Button variant="outline" size="sm" onClick={load} disabled={loading}>
            {intl.formatMessage({ id: 'security.credentialInventory.refresh' })}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

/** Sources that live outside `config.toml` — the ones a rotation never touches
 *  the config file for. Mirrors the backend's `referenced` count. */
function isExternalReference(source: CredentialSourceKind): boolean {
  return (
    source === 'env' ||
    source === 'keychain' ||
    source === 'file' ||
    source === 'vault' ||
    source === 'onepassword' ||
    source === 'infisical' ||
    source === 'local'
  );
}

const sourceIcon: Record<CredentialSourceKind, typeof Lock> = {
  unset: HelpCircle,
  inline: Lock,
  legacy: AlertTriangle,
  env: Terminal,
  keychain: KeyRound,
  file: FileText,
  vault: Cloud,
  onepassword: Cloud,
  infisical: Cloud,
  local: Cloud,
  ambiguous: HelpCircle,
};

function toneFor(entry: CredentialEntry): string {
  if (entry.residue) return 'bg-destructive/10 text-destructive';
  if (entry.source === 'legacy') return 'bg-warning/15 text-warning';
  if (isExternalReference(entry.source)) return 'bg-success/15 text-success';
  if (entry.source === 'unset') return 'bg-secondary text-secondary-foreground';
  return 'bg-secondary text-secondary-foreground';
}

function InventoryRow({ entry }: { entry: CredentialEntry }) {
  const intl = useIntl();
  const Icon = sourceIcon[entry.source] ?? HelpCircle;
  return (
    <div className="flex items-center justify-between gap-2 rounded-lg border border-surface-border bg-card px-3 py-2 text-sm">
      <code className="min-w-0 flex-1 truncate font-mono text-xs text-foreground">
        {entry.path}
      </code>
      <div className="flex shrink-0 items-center gap-1.5">
        {entry.residue && (
          <Badge variant="secondary" className="bg-destructive/10 text-destructive">
            {intl.formatMessage({ id: 'security.credentialInventory.badge.residue' })}
          </Badge>
        )}
        <Badge variant="secondary" className={toneFor(entry)}>
          <Icon className="mr-1 size-3" aria-hidden />
          {intl.formatMessage({ id: `security.credentialInventory.source.${entry.source}` })}
        </Badge>
        {/* The label is non-secret by construction (a variable name, a keychain
            entry name, a vault path) — never the value or the ciphertext. */}
        {isExternalReference(entry.source) && (
          <code className="hidden max-w-[16rem] truncate font-mono text-xs text-muted-foreground sm:inline">
            {entry.source_label}
          </code>
        )}
      </div>
    </div>
  );
}
