import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { useParams, useSearchParams } from 'react-router';
import { Button } from '@/components/mds';
import { client } from '@/lib/ws-client';
import { ComputerWatchPanel } from '@/components/agent/ComputerWatchPanel';

export function ComputerWatchPage() {
  const { agentId } = useParams<{ agentId: string }>();
  const [search] = useSearchParams();
  if (!agentId || !/^[A-Za-z0-9_-]+$/.test(agentId) || agentId.length > 128) return null;
  const token = search.get('l');
  return <WatchContext key={`${agentId}:${token ?? ''}`} agentId={agentId} token={token} />;
}

function WatchContext({ agentId, token }: { agentId: string; token: string | null }) {
  const intl = useIntl();
  const [, setSearch] = useSearchParams();
  const [state, setState] = useState<'loading' | 'ready' | 'expired'>(token === null ? 'ready' : 'loading');
  const [session, setSession] = useState<string>();
  const expired = useCallback(() => setState('expired'), []);
  useEffect(() => {
    if (token === null) return;
    let alive = true;
    if (!token || token.length > 1024 || /[\x00-\x20]/.test(token)) {
      setState('expired');
      return;
    }
    void client.call('computer_sessions.open_link', { token }).then((raw: unknown) => {
      if (!alive) return;
      const value = raw as Record<string, unknown> | null;
      if (!value || value.ok !== true || value.agent_id !== agentId
        || typeof value.session_id !== 'string' || !/^[A-Za-z0-9-]{1,128}$/.test(value.session_id)) {
        setState('expired');
        return;
      }
      setSession(value.session_id); setState('ready');
    }).catch(() => { if (alive) setState('expired'); });
    return () => { alive = false; };
  }, [agentId, token]);
  if (state === 'loading') return <p role="status" className="p-6 text-sm text-muted-foreground">{intl.formatMessage({ id: 'computerWatch.loading' })}</p>;
  if (state === 'expired') return <section className="space-y-4 p-6">
    <p role="alert">{intl.formatMessage({ id: 'computerWatch.expired' })}</p>
    <Button variant="outline" onClick={() => setSearch((old) => { const next = new URLSearchParams(old); next.delete('l'); return next; }, { replace: true })}>{intl.formatMessage({ id: 'computerWatch.current' })}</Button>
  </section>;
  return <ComputerWatchPanel agentId={agentId} expectedSession={session} onExpired={expired} />;
}
