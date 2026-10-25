import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type GoogleDelegationStatus } from '@/lib/api';
import { useSystemStore } from '@/stores/system-store';
import { toast } from '@/lib/toast';
import { Button, Card, CardContent, CardHeader, CardTitle, Input } from '@/components/mds';

export function GooglePersonalAdminPanel() {
  const intl = useIntl();
  const t = (id:string) => intl.formatMessage({id});
  const enterprise = useSystemStore(s=>s.status?.edition_profile === 'enterprise');
  const [status,setStatus] = useState<GoogleDelegationStatus|null>(null);
  const [domains,setDomains] = useState('');
  const [busy,setBusy] = useState(false);
  const load = useCallback(async()=>{ try{const next=await api.googleIdentity.delegation();setStatus(next);setDomains(next.settings?.allowed_domains?.join(', ')??'');}catch{setStatus(null);}},[]);
  useEffect(()=>{if(enterprise)void load();},[enterprise,load]);
  if(!enterprise||!status)return null;
  const run = async(action:()=>Promise<unknown>)=>{setBusy(true);try{await action();await load();}catch(e){toast.error(e instanceof Error?e.message:String(e));}finally{setBusy(false);}};
  return <Card><CardHeader><CardTitle>{t('googlePersonal.adminTitle')}</CardTitle></CardHeader><CardContent className="space-y-3">
    <label htmlFor="personal-google-domains" className="text-sm">{t('googlePersonal.domains')}</label><Input id="personal-google-domains" value={domains} onChange={e=>setDomains(e.target.value)}/><p className="text-xs text-muted-foreground">{t('googlePersonal.domainHint')}</p>
    <Button disabled={busy||!!status.settings_error} onClick={()=>void run(()=>api.googleIdentity.setSettings({allowed_domains:domains.split(/[,\s]+/).filter(Boolean)}))}>{t('common.save')}</Button>
    <p className="text-sm">{t('myConnections.status.connected')}: {status.personal_connections?.filter(c=>c.status==='connected').length??0} · {t('myConnections.status.needs_reconnect')}: {status.personal_connections?.filter(c=>c.status==='needs_reconnect').length??0}</p>
    {status.personal_connections===null&&<p className="text-warning">{t('common.error')}</p>}
    {status.personal_connections?.map(connection=><div className="flex flex-wrap items-center justify-between gap-3 text-sm" key={connection.owner_id}><div>{connection.account_email} · {t(`myConnections.status.${connection.status}`)}</div><Button variant="outline" disabled={busy} onClick={()=>{if(window.confirm(t('myConnections.disconnectConfirm')))void run(()=>api.personalGoogle.disconnectUser(connection.owner_id));}}>{t('myConnections.disconnect')}</Button></div>)}
  </CardContent></Card>;
}
