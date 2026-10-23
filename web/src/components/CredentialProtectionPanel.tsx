import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { ShieldCheck, ShieldAlert } from 'lucide-react';
import {
  api,
  type CredentialProtectionState,
  type CredentialProtectionStatus,
} from '@/lib/api';
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
import { ConfirmDialog } from '@/components/settings/controls';
import { toast, formatError } from '@/lib/toast';

/**
 * CredentialProtectionPanel (B2c) — one line: where the credentials that let
 * an AI employee act as a specific person are protected (OS keychain or
 * file), plus the Admin actions to move them into the keychain or back.
 *
 * Text is rendered from `state` + `level` through the locale catalogue, not
 * from the backend's zh-TW summary, so every locale reads its own language.
 * The payload never carries key material (only a key id).
 */
const BROKEN: readonly CredentialProtectionState[] = ['missing', 'key_changed', 'invalid'];

export function CredentialProtectionPanel() {
  const intl = useIntl();
  const [status, setStatus] = useState<CredentialProtectionStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<'reset' | 'disable' | null>(null);

  const load = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    api.security
      .credentialProtection()
      .then(setStatus)
      .catch((e) => {
        setLoadError(e);
        setStatus(null);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const run = useCallback(
    async (action: 'enable' | 'reset' | 'disable') => {
      setBusy(true);
      try {
        const next =
          action === 'disable'
            ? await api.security.credentialProtectionDisable()
            : await api.security.credentialProtectionEnable(action === 'reset');
        setStatus(next);
        toast.success(
          intl.formatMessage({
            id: action === 'disable'
              ? 'security.credentialProtection.toast.disabled'
              : 'security.credentialProtection.toast.enabled',
          }),
        );
      } catch (e) {
        toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(e) }));
      } finally {
        setBusy(false);
      }
    },
    [intl],
  );

  const state = status?.state;
  const ready = state === 'ready';
  const label = status
    ? state === 'ready' || state === 'not_enabled'
      ? intl.formatMessage({ id: `security.credentialProtection.level.${status.level}` })
      : intl.formatMessage({ id: `security.credentialProtection.state.${state}` })
    : '';

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          {ready ? (
            <ShieldCheck className="size-4 text-success" />
          ) : (
            <ShieldAlert className="size-4 text-warning" />
          )}
          {intl.formatMessage({ id: 'security.credentialProtection.title' })}
        </CardTitle>
        <CardDescription>
          {intl.formatMessage({ id: 'security.credentialProtection.desc' })}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {loadError != null ? (
          <ErrorState
            variant="inline"
            error={loadError}
            title={intl.formatMessage({ id: 'security.credentialProtection.loadErrorTitle' })}
            onRetry={load}
          />
        ) : loading || !status ? (
          <Skeleton className="h-12 w-full" />
        ) : (
          <div className="space-y-3">
            <p className="text-sm font-medium text-foreground" data-testid="credential-protection-line">
              {intl.formatMessage({ id: 'security.credentialProtection.line' }, { label })}
            </p>
            <p className="text-sm text-muted-foreground">
              {intl.formatMessage({
                id: ready
                  ? `security.credentialProtection.detail.ready.${status.level}${
                      status.level === 'user_store' && status.backend === 'macos_keychain' ? '_macos' : ''
                    }`
                  : `security.credentialProtection.detail.${state}`,
              })}
            </p>
            {state === 'needs_authorization' && (
              <code className="block rounded-md bg-secondary px-3 py-2 font-mono text-xs">
                duduclaw credentials authorize
              </code>
            )}
            <Badge
              variant="secondary"
              className={status.person_identity_allowed ? 'bg-success/15 text-success' : 'bg-secondary'}
            >
              {intl.formatMessage({
                id: status.person_identity_allowed
                  ? 'security.credentialProtection.personAllowed'
                  : 'security.credentialProtection.personNotAllowed',
              })}
            </Badge>
            <div className="flex flex-wrap justify-end gap-2 border-t border-surface-border pt-3">
              {state === 'not_enabled' && (
                <Button size="sm" onClick={() => void run('enable')} disabled={busy}>
                  {intl.formatMessage({ id: 'security.credentialProtection.enable' })}
                </Button>
              )}
              {ready && (
                <Button size="sm" variant="outline" onClick={() => setConfirm('reset')} disabled={busy}>
                  {intl.formatMessage({ id: 'security.credentialProtection.rotate' })}
                </Button>
              )}
              {state && BROKEN.includes(state) && (
                <Button size="sm" variant="outline" onClick={() => setConfirm('reset')} disabled={busy}>
                  {intl.formatMessage({ id: 'security.credentialProtection.reset' })}
                </Button>
              )}
              {state && state !== 'not_enabled' && state !== 'unsupported' && (
                <Button size="sm" variant="ghost" onClick={() => setConfirm('disable')} disabled={busy}>
                  {intl.formatMessage({ id: 'security.credentialProtection.disable' })}
                </Button>
              )}
              <Button size="sm" variant="outline" onClick={load} disabled={loading || busy}>
                {intl.formatMessage({ id: 'security.credentialProtection.refresh' })}
              </Button>
            </div>
          </div>
        )}
      </CardContent>

      <ConfirmDialog
        open={confirm !== null}
        onClose={() => setConfirm(null)}
        onConfirm={() => {
          const action = confirm;
          setConfirm(null);
          if (action) void run(action);
        }}
        title={intl.formatMessage({ id: `security.credentialProtection.confirm.${confirm ?? 'disable'}.title` })}
        message={intl.formatMessage({ id: `security.credentialProtection.confirm.${confirm ?? 'disable'}.message` })}
        confirmLabel={intl.formatMessage({
          id: confirm === 'reset' ? 'security.credentialProtection.reset' : 'security.credentialProtection.disable',
        })}
        requireText={
          confirm === 'disable'
            ? intl.formatMessage({ id: 'security.credentialProtection.confirm.disable.typed' })
            : undefined
        }
        busy={busy}
      />
    </Card>
  );
}
