import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { ShieldCheck, ShieldAlert, KeyRound } from 'lucide-react';
import { api, type CredentialFinding } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Badge,
  Button,
  CrossLink,
  ErrorState,
  Skeleton,
} from '@/components/mds';

const severityToneClass: Record<string, string> = {
  high: 'bg-destructive/10 text-destructive',
  medium: 'bg-warning/15 text-warning',
  low: 'bg-secondary text-secondary-foreground',
  info: 'bg-secondary text-secondary-foreground',
};

/**
 * CredentialHygienePanel (WP-K) — the dashboard's friendly counterpart to
 * `security_posture::find_plaintext_secrets` / `strip_twin_residue`.
 *
 * Surfaces the 2026-08-15 incident class (a plaintext `[[accounts]]
 * oauth_token` sitting next to its already-authoritative `oauth_token_enc`
 * twin — commercial/docs/DESIGN-credentials-doctrine-2026-08.md §1.5) as
 * something an operator can see and act on, instead of something only
 * discoverable by hand-reading config.toml:
 *
 *  - Green when clean.
 *  - A per-field list when not, split into "safe to auto-clean" (has an
 *    encrypted twin already) and "needs manual attention" (no twin — this
 *    pass never guesses how to encrypt an unfamiliar field).
 *  - A confirmed one-click cleanup that only ever touches the safe set, and
 *    a plain-language pointer to the Accounts page for rotating the token
 *    that caused the residue in the first place.
 *
 * Never renders a secret value — the backend RPCs don't return one to begin
 * with (paths + booleans + severity only).
 */
export function CredentialHygienePanel({
  onGoToAccounts,
}: {
  onGoToAccounts: () => void;
}) {
  const intl = useIntl();
  const [loading, setLoading] = useState(true);
  const [findings, setFindings] = useState<CredentialFinding[] | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [cleaning, setCleaning] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);

  const load = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    api.security
      .credentialHygiene()
      .then((res) => setFindings(res.findings))
      .catch((e) => {
        setLoadError(e);
        setFindings(null);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const cleanable = (findings ?? []).filter((f) => f.has_enc_twin);
  const manualOnly = (findings ?? []).filter((f) => !f.has_enc_twin);

  const handleCleanup = async () => {
    setCleaning(true);
    try {
      const res = await api.security.credentialCleanup();
      if (res.cleaned) {
        toast.success(
          intl.formatMessage(
            { id: 'security.credentialHygiene.cleanupSuccess' },
            { count: res.removed_paths.length },
          ),
        );
      } else {
        toast.success(intl.formatMessage({ id: 'security.credentialHygiene.cleanupNoop' }));
      }
      load();
    } catch (e) {
      toast.error(
        intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }),
      );
    } finally {
      setCleaning(false);
    }
  };

  const clean = findings != null && findings.length === 0;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <KeyRound className="size-4 text-brand" />
          {intl.formatMessage({ id: 'security.credentialHygiene.title' })}
        </CardTitle>
        <CardDescription>
          {intl.formatMessage({ id: 'security.credentialHygiene.desc' })}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {loadError != null ? (
          <ErrorState
            variant="inline"
            error={loadError}
            title={intl.formatMessage({ id: 'security.credentialHygiene.loadErrorTitle' })}
            onRetry={load}
          />
        ) : loading ? (
          <div className="space-y-2">
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-9 w-full" />
          </div>
        ) : clean ? (
          <div className="flex items-center gap-2 rounded-lg bg-success/10 px-3 py-2.5 text-sm text-success">
            <ShieldCheck className="size-4 shrink-0" />
            {intl.formatMessage({ id: 'security.credentialHygiene.clean' })}
          </div>
        ) : (
          <div className="space-y-4">
            <div className="flex items-center gap-2 rounded-lg bg-warning/10 px-3 py-2.5 text-sm text-warning">
              <ShieldAlert className="size-4 shrink-0" />
              {intl.formatMessage(
                { id: 'security.credentialHygiene.foundSummary' },
                { count: findings?.length ?? 0 },
              )}
            </div>

            {cleanable.length > 0 && (
              <div className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage(
                    { id: 'security.credentialHygiene.cleanableSummary' },
                    { count: cleanable.length },
                  )}
                </p>
                {cleanable.map((f) => (
                  <FindingRow key={f.path} finding={f} cleanable />
                ))}
              </div>
            )}

            {manualOnly.length > 0 && (
              <div className="space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  {intl.formatMessage(
                    { id: 'security.credentialHygiene.manualSummary' },
                    { count: manualOnly.length },
                  )}
                </p>
                {manualOnly.map((f) => (
                  <FindingRow key={f.path} finding={f} cleanable={false} />
                ))}
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'security.credentialHygiene.manualHint' })}
                </p>
              </div>
            )}
          </div>
        )}

        <div className="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-surface-border pt-3">
          <div className="flex flex-wrap items-center gap-2">
            <p className="text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'security.credentialHygiene.rotateGuidanceHint' })}
            </p>
            <CrossLink
              label={intl.formatMessage({ id: 'security.credentialHygiene.rotateGuidanceLink' })}
              onClick={onGoToAccounts}
            />
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={load} disabled={loading}>
              {intl.formatMessage({ id: 'security.credentialHygiene.refresh' })}
            </Button>
            <Button
              variant="brand"
              size="sm"
              onClick={() => setConfirmOpen(true)}
              disabled={loading || cleaning || cleanable.length === 0}
            >
              {intl.formatMessage({ id: 'security.credentialHygiene.cleanupButton' })}
            </Button>
          </div>
        </div>
      </CardContent>

      <ConfirmDialog
        open={confirmOpen}
        onClose={() => setConfirmOpen(false)}
        onConfirm={() => {
          setConfirmOpen(false);
          void handleCleanup();
        }}
        title={intl.formatMessage({ id: 'security.credentialHygiene.confirmTitle' })}
        message={intl.formatMessage(
          { id: 'security.credentialHygiene.confirmMessage' },
          { count: cleanable.length },
        )}
        confirmLabel={intl.formatMessage({ id: 'security.credentialHygiene.confirmButton' })}
        busy={cleaning}
      />
    </Card>
  );
}

function FindingRow({
  finding,
  cleanable,
}: {
  finding: CredentialFinding;
  cleanable: boolean;
}) {
  const intl = useIntl();
  return (
    <div className="flex items-center justify-between gap-2 rounded-lg border border-surface-border bg-card px-3 py-2 text-sm">
      <code className="min-w-0 truncate font-mono text-xs text-foreground">{finding.path}</code>
      <div className="flex shrink-0 items-center gap-1.5">
        <Badge
          variant="secondary"
          className={severityToneClass[finding.severity] ?? severityToneClass.info}
        >
          {intl.formatMessage({ id: `security.credentialHygiene.severity.${finding.severity}` })}
        </Badge>
        <Badge
          variant="secondary"
          className={cleanable ? 'bg-success/15 text-success' : 'bg-warning/15 text-warning'}
        >
          {intl.formatMessage({
            id: cleanable
              ? 'security.credentialHygiene.finding.cleanableBadge'
              : 'security.credentialHygiene.finding.manualBadge',
          })}
        </Badge>
      </div>
    </div>
  );
}
