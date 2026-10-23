import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link, useSearchParams } from 'react-router';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { toast } from '@/lib/toast';
import { Button, Card, CardContent, CardHeader, CardTitle, ErrorState, Input, Skeleton } from '@/components/mds';

export function MyConnectionsPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string,string|number>) => intl.formatMessage({id},values);
  const [query, setQuery] = useSearchParams();
  const finishing = useRef<string|null>(null);
  const [status,setStatus] = useState<PersonalGoogleStatus|null>(null);
  const [error,setError] = useState<unknown>(null);
  const [busy,setBusy] = useState(false);
  const [paste,setPaste] = useState(false);
  const [redirectUri,setRedirectUri] = useState('');
  const [url,setUrl] = useState('');
  const [confirmed,setConfirmed] = useState<Record<string,boolean>>({});
  const [disconnectConfirmed,setDisconnectConfirmed] = useState(false);
  const [disconnectNote,setDisconnectNote] = useState('');
  const load = useCallback(async () => {
    try { setStatus(await api.personalGoogle.status()); setError(null); } catch(e) { setError(e); }
  },[]);
  useEffect(() => { void load(); const focus = () => void load(); window.addEventListener('focus',focus); return () => window.removeEventListener('focus',focus); },[load]);
  const run = useCallback(async (action:()=>Promise<unknown>) => {
    setBusy(true);
    try { await action(); await load(); } catch(e) { toast.error(e instanceof Error ? e.message : String(e)); } finally { setBusy(false); }
  },[load]);
  useEffect(() => {
    const state = query.get('google_state');
    if (!state || finishing.current === state) return;
    finishing.current = state;
    // Remove the nonce from browser history immediately; the code never
    // reaches this page and authentication remains in the normal WS client.
    setQuery({}, {replace:true});
    void run(()=>api.personalGoogle.finish(state));
  },[query,setQuery,run]);
  const connect = async () => {
    const popup = window.open('about:blank','_blank');
    if (popup) popup.opener = null;
    setBusy(true);
    try { const started = await api.personalGoogle.start(); setRedirectUri(started.redirect_uri); setPaste(started.completion === 'paste'); if (popup) popup.location.replace(started.authorization_url); else window.location.assign(started.authorization_url); }
    catch(e) { popup?.close(); toast.error(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  if (error) return <ErrorState error={error} onRetry={()=>void load()} />;
  if (!status) return <Skeleton className="h-40 w-full" />;
  return <div className="mx-auto max-w-4xl space-y-6 p-6">
    <div><h1 className="text-2xl font-semibold">{t('myConnections.title')}</h1><p className="mt-2 text-sm text-muted-foreground">{t('myConnections.intro')}</p></div>
    <Card><CardHeader><CardTitle>Google</CardTitle></CardHeader><CardContent className="space-y-3">
      {status.connection && <><p className="font-medium">{status.connection.account_email}</p><p className="text-sm">{t(`myConnections.status.${status.connection.status}`)}</p>
        <Link to="/mail" className="text-sm underline">{t('nav.mail')}</Link>
        {status.connection.account_email.toLowerCase() !== status.login_email.toLowerCase() && <p className="text-sm text-warning">{t('myConnections.different',{email:status.login_email})}</p>}
        <p className="text-sm">{t('myConnections.connectedAt',{time:new Date(status.connection.connected_at).toLocaleString(intl.locale)})}</p>
        <p className="text-sm">{t('myConnections.services')}: {['gmail','calendar','drive','spreadsheets','documents','presentations','forms','tasks'].filter(service=>status.connection!.granted_scopes.some(scope=>scope.includes(service))).map(service=>t(`myConnections.service.${service}`)).join('、') || t('myConnections.noServices')}</p>
      </>}
      {!status.client_configured && <p>{t('myConnections.noClient')}</p>}
      {!status.integration_enabled && <p className="text-sm text-warning">{t('myConnections.integrationOff')}</p>}
      {status.requires_login && <p className="text-sm text-warning">{t('myConnections.loginAgain')}</p>}
      <p className="text-xs text-muted-foreground">{t('myConnections.testing')}</p>
      <Button disabled={busy||!status.client_configured||status.requires_login} onClick={()=>void connect()}>{status.connection ? t('myConnections.reconnect') : t('myConnections.connect')}</Button>
      {redirectUri&&<div className="space-y-2"><label htmlFor="google-registered-return">{t('myConnections.registeredReturn')}</label><Input id="google-registered-return" readOnly value={redirectUri}/><Button variant="outline" onClick={()=>void navigator.clipboard.writeText(redirectUri).catch(e=>toast.error(e instanceof Error?e.message:String(e)))}>{t('myConnections.copyReturn')}</Button></div>}
      {disconnectNote&&<p className="text-sm text-warning">{disconnectNote}</p>}
      {status.connection && <div className="space-y-2"><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={disconnectConfirmed} onChange={e=>setDisconnectConfirmed(e.target.checked)}/>{t('myConnections.disconnectConfirm')}</label><Button variant="outline" disabled={busy||!disconnectConfirmed} onClick={()=>void run(async()=>{const result=await api.personalGoogle.disconnect();setDisconnectNote(result.provider_note??'');})}>{t('myConnections.disconnect')}</Button></div>}
      {paste && <div className="space-y-2"><p className="text-sm">{t('myConnections.pasteHint')}</p><label htmlFor="google-return-url">{t('myConnections.returnUrl')}</label><Input id="google-return-url" type="url" value={url} onChange={e=>setUrl(e.target.value)} autoComplete="off"/><Button disabled={busy||!url} onClick={()=>void run(async()=>{await api.personalGoogle.paste(url);setUrl('');setPaste(false);})}>{t('myConnections.finish')}</Button></div>}
    </CardContent></Card>
    <Card><CardHeader><CardTitle>{t('myConnections.employees')}</CardTitle></CardHeader><CardContent className="space-y-4">
      {status.requests.length===0 && <p className="text-sm text-muted-foreground">{t('myConnections.noEmployees')}</p>}
      {status.requests.map(request=><div key={request.agent_id} className="space-y-2 rounded border border-border p-3">
        <Link className="font-medium underline" to={`/agents/${encodeURIComponent(request.agent_id)}/connections`}>{request.agent_id}</Link>
        <p className="text-sm">{request.allowed===null ? t('myConnections.awaiting') : request.allowed ? t('myConnections.allowed') : t('myConnections.declined')}</p>
        <p className="text-sm">{t(`myConnections.requesters.${request.requesters}`)} {request.bound_people.map(person=>`${person.name} (${person.email})`).join('、')}</p>
        <p className="text-xs text-muted-foreground">{t('myConnections.channels',{channels:request.channels.join('、')||t('myConnections.noChannels')})} {t('myConnections.publicDenied')}</p>
        {request.allowed!==true && <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={confirmed[request.agent_id]??false} onChange={e=>setConfirmed({...confirmed,[request.agent_id]:e.target.checked})}/>{t('myConnections.sharedConfirm')}</label>}
        <div className="flex gap-2">{request.allowed!==true && <Button disabled={busy||!confirmed[request.agent_id]} onClick={()=>void run(()=>api.personalGoogle.consent(request.agent_id,true))}>{t('myConnections.allow')}</Button>}<Button variant="outline" disabled={busy||request.allowed===false} onClick={()=>void run(()=>api.personalGoogle.consent(request.agent_id,false))}>{t('myConnections.deny')}</Button></div>
      </div>)}
    </CardContent></Card>
    <Card><CardHeader><CardTitle>{t('myConnections.delegated')}</CardTitle></CardHeader><CardContent><p className="text-sm text-muted-foreground">{t('myConnections.adminManaged')}</p>{status.delegated.map(row=><p key={row.agent_id}>{row.agent_id} — {row.email}</p>)}</CardContent></Card>
    <Card><CardHeader><CardTitle>{t('myConnections.history')}</CardTitle></CardHeader><CardContent><p className="text-xs text-muted-foreground">{t('myConnections.historyHint')}</p>{status.recent_usage.map((row,index)=><div className="mt-2 flex flex-wrap gap-3 text-sm" key={`${row.timestamp}-${index}`}><time>{new Date(row.timestamp).toLocaleString(intl.locale)}</time><span>{row.agent_id}</span><span>{row.tool_name}</span><span>{t(row.success?'myConnections.success':'myConnections.failure')}</span></div>)}</CardContent></Card>
  </div>;
}
