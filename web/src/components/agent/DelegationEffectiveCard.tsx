import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { api, type DelegationEffective } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { hasMinRole } from '@/lib/roles';
import { Badge, Card, CardContent, CardHeader, CardTitle, useErrorMessage } from '@/components/mds';

const POLICIES: ReadonlyArray<string> = ['department', 'hierarchy', 'open', 'none'];
/**
 * `delegation.effective.config_error` categories with their own message. Any
 * other value (an older gateway's raw text, a future category) renders the
 * generic `unknown` message — config text is never shown here.
 */
const CONFIG_ERROR_KINDS: ReadonlyArray<string> = ['unreadable', 'unparsable', 'permissions_unreadable'];
/** RM-137 L1: the gateway's masked-peer placeholder (`HIDDEN_PEER_PLACEHOLDER`). */
export const DELEGATION_HIDDEN_PEER = '*hidden*';
const isPairList = (v: unknown) =>
  Array.isArray(v) && v.every((p) => Array.isArray(p) && p.length === 2 && p.every((x) => typeof x === 'string'));

export function isDelegationEffective(v: unknown): v is DelegationEffective {
  if (!v || typeof v !== 'object') return false;
  const d = v as Record<string, unknown>;
  return typeof d.policy === 'string' && POLICIES.includes(d.policy)
    && (d.source === 'global' || d.source === 'agent_permissions')
    && typeof d.may_delegate === 'boolean' && typeof d.may_create_agents === 'boolean'
    && isPairList(d.allow_pairs) && isPairList(d.inactive_allow_pairs)
    && (d.allow_warning_count === undefined || typeof d.allow_warning_count === 'number');
}

/**
 * RM-137 — read-only summary of what delegation the gateway applies to one
 * employee (`delegation.effective`): the effective policy and where it comes
 * from (the org-wide setting or this employee's own switch), the
 * cross-department pairs naming it, a config problem when there is one (as a
 * fixed message per category — never the config text), and
 * how many `[delegation] allow` entries were dropped (count only — the texts
 * stay in Settings, Admin-only).
 * Changing it happens in 設定 › 委派權限 (Admin) or on the edit page.
 *
 * Shown to Manager and up — the gateway gates the RPC the same way (plus
 * view access to this employee); a refusal renders the error, never a guess.
 */
export function DelegationEffectiveCard({ agentId }: { agentId: string }) {
  const role = useAuthStore((s) => s.user?.role);
  const connected = useConnectionStore((s) => s.state === 'authenticated');
  if (!hasMinRole(role, 'manager') || !connected) return null;
  return <DelegationEffectiveContent key={agentId} agentId={agentId} />;
}

export function DelegationEffectiveContent({ agentId }: { agentId: string }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const errorText = useErrorMessage();
  const [data, setData] = useState<DelegationEffective | null>(null);
  const [error, setError] = useState<unknown>(null);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    // Through a promise so an unavailable client surfaces as the error line.
    Promise.resolve()
      .then(() => api.delegation.effective(agentId))
      .then((next) => {
        // Shown only when it is the shape the gateway sends; anything else
        // (an older gateway) is an error, never a guessed policy.
        if (!isDelegationEffective(next)) throw new Error('delegation.effective: unexpected response');
        if (alive.current) { setData(next); setError(null); }
      })
      .catch((e) => { if (alive.current) setError(e); });
    return () => { alive.current = false; };
  }, [agentId]);

  // RM-137 L1: the gateway masks a peer this viewer cannot view; render it as
  // words, never as the raw placeholder.
  const hidden = data?.hidden_peer_placeholder || DELEGATION_HIDDEN_PEER;
  const peer = (id: string) => (id === hidden ? t('agentDetail.delegation.hiddenPeer') : id);
  const pairText = (pairs: Array<[string, string]>) =>
    pairs.map(([a, b]) => `${peer(a)} ⇄ ${peer(b)}`).join('、');
  // RM-137 re-review: `config_error` is a category, never text; the detail
  // (`config_error_detail`, Admin only) can quote raw config and stays out of
  // this card — an Admin reads it in Settings › Delegation permissions.
  const configErrorText = (kind: string) =>
    t(`agentDetail.delegation.configError.${CONFIG_ERROR_KINDS.includes(kind) ? kind : 'unknown'}`);

  return (
    <Card data-size="sm">
      <CardHeader>
        <CardTitle className="text-sm">{t('agentDetail.delegation.title')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2 text-sm">
        {data && (
          <>
            <p className="flex flex-wrap items-center gap-2">
              <span className="font-medium text-foreground">
                {t(`settings.delegation.policy.${data.policy}`)}
              </span>
              <Badge
                variant="secondary"
                className={data.may_delegate ? 'bg-success/15 text-success' : undefined}
              >
                {t(data.may_delegate ? 'agentDetail.delegation.mayDelegate' : 'agentDetail.delegation.mayNotDelegate')}
              </Badge>
            </p>
            <p className="text-xs text-muted-foreground">
              {t(`agentDetail.delegation.source.${data.source}`)}
            </p>
            <p className="text-xs text-muted-foreground">
              {t(data.may_create_agents ? 'agentDetail.delegation.mayCreate' : 'agentDetail.delegation.mayNotCreate')}
            </p>
            {data.allow_pairs.length > 0 && (
              <p className="text-xs text-muted-foreground">
                {t('agentDetail.delegation.pairs', { pairs: pairText(data.allow_pairs) })}
              </p>
            )}
            {data.inactive_allow_pairs.length > 0 && (
              <p className="text-xs text-muted-foreground">
                {t('agentDetail.delegation.inactivePairs', { pairs: pairText(data.inactive_allow_pairs) })}
              </p>
            )}
            {data.policy === 'none' && (
              <p className="text-xs text-muted-foreground">{t('agentDetail.delegation.teamNote')}</p>
            )}
            {(data.config_error || data.policy_warning) && (
              <p role="alert" className="flex items-start gap-1.5 text-xs text-warning">
                <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                <span>
                  {t('agentDetail.delegation.configProblem')}
                  {' '}
                  {data.config_error ? configErrorText(data.config_error) : data.policy_warning}
                </span>
              </p>
            )}
            {/* RM-137 re-review L: only the count — the warning texts quote raw
                allow entries, which may name employees this viewer cannot view. */}
            {(data.allow_warning_count ?? 0) > 0 && (
              <p className="flex items-start gap-1.5 text-xs text-warning">
                <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                <span>{t('agentDetail.delegation.allowWarnings', { count: data.allow_warning_count ?? 0 })}</span>
              </p>
            )}
            <p className="text-xs text-muted-foreground">{t('agentDetail.delegation.readOnly')}</p>
          </>
        )}
        {!data && error == null && <p role="status">{t('common.loading')}</p>}
        {error != null && (
          <p role="alert" className="text-destructive">{errorText(error)}</p>
        )}
      </CardContent>
    </Card>
  );
}
