import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type McpOAuthGrant } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { toast, formatError } from '@/lib/toast';
import {
  Button, Empty, ErrorState, Dialog, DialogContent, DialogHeader, DialogTitle,
  DialogFooter, DialogClose,
} from '@/components/mds';

export function OAuthGrantsPanel() {
  const user = useAuthStore((s) => s.user);
  if (user?.role !== 'admin') return null;
  return <AdminOAuthGrants key={user.id} />;
}

function AdminOAuthGrants() {
  const intl = useIntl();
  const [grants, setGrants] = useState<McpOAuthGrant[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<unknown>(null);
  const [revision, setRevision] = useState(0);
  const [selected, setSelected] = useState<McpOAuthGrant | null>(null);
  const [revoking, setRevoking] = useState(false);
  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    api.mcpOAuthGrants.list().then(
      (result) => { if (active) setGrants(result.grants); },
      (failure: unknown) => { if (active) setError(failure); },
    ).finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [revision]);

  const revoke = async () => {
    if (!selected) return;
    setRevoking(true);
    try {
      await api.mcpOAuthGrants.revoke(selected.id);
      setSelected(null);
      setRevision((n) => n + 1);
      toast.success(intl.formatMessage({ id: 'mcpKeys.oauth.revoked' }));
    } catch (failure) {
      toast.error(intl.formatMessage({ id: 'toast.error.actionFailed' }, { message: formatError(failure) }));
    } finally {
      setRevoking(false);
    }
  };
  const date = (unix: number) => intl.formatDate(unix * 1000, {
    year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  });
  return (
    <section className="space-y-3" aria-labelledby="mcp-oauth-grants-title">
      <h2 id="mcp-oauth-grants-title" className="text-base font-semibold">
        {intl.formatMessage({ id: 'mcpKeys.oauth.title' })}
      </h2>
      <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'mcpKeys.oauth.desc' })}</p>
      {loading ? <p role="status">{intl.formatMessage({ id: 'common.loading' })}</p>
        : error != null ? <ErrorState error={error} onRetry={() => setRevision((n) => n + 1)} />
        : grants.length === 0 ? <Empty title={intl.formatMessage({ id: 'mcpKeys.oauth.empty' })} variant="dashed" />
        : <ul className="divide-y divide-surface-border rounded-xl border border-surface-border">
          {grants.map((grant) => <li key={grant.id} className="flex flex-wrap items-center justify-between gap-4 p-4">
            <div className="min-w-0 space-y-1">
              <h3 className="break-words font-medium">{grant.client_name}</h3>
              <p className="break-words text-sm text-muted-foreground">{grant.scopes.join(', ')}</p>
              <dl className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                <div><dt className="inline">{intl.formatMessage({ id: 'mcpKeys.oauth.issued' })}: </dt><dd className="inline">{date(grant.issued_unix)}</dd></div>
                <div><dt className="inline">{intl.formatMessage({ id: 'mcpKeys.oauth.lastUsed' })}: </dt><dd className="inline">{grant.last_used_unix == null ? intl.formatMessage({ id: 'mcpKeys.oauth.neverUsed' }) : date(grant.last_used_unix)}</dd></div>
              </dl>
            </div>
            <Button variant="outline" size="sm" onClick={() => setSelected(grant)}>
              {intl.formatMessage({ id: 'mcpKeys.revoke' })}
            </Button>
          </li>)}
        </ul>}
      <Dialog open={selected !== null} onOpenChange={(open) => { if (!open && !revoking) setSelected(null); }}>
        <DialogContent>
          <DialogHeader><DialogTitle>{intl.formatMessage({ id: 'mcpKeys.oauth.revokeTitle' })}</DialogTitle></DialogHeader>
          <p>{intl.formatMessage({ id: 'mcpKeys.oauth.confirm' }, { client: selected?.client_name ?? '' })}</p>
          <DialogFooter>
            <DialogClose render={<Button variant="outline" disabled={revoking}>{intl.formatMessage({ id: 'common.cancel' })}</Button>} />
            <Button variant="destructive" onClick={() => void revoke()} disabled={revoking}>
              {intl.formatMessage({ id: revoking ? 'common.loading' : 'mcpKeys.revoke' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
