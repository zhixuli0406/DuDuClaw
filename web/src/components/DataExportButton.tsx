import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download } from 'lucide-react';
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, Input } from '@/components/mds';
import { api } from '@/lib/api';
import { dataExports, downloadDataExport, exportDateRange, type ExportDataset, type ExportFormat, type ExportJob } from '@/lib/data-exports';

const datasets: ExportDataset[] = ['tasks','conversations','usage','memories','wiki'];
const knownErrors = new Set(['EXPORT_PERMISSION_DENIED','EXPORT_SOURCE_CHANGED','EXPORT_BUSY','EXPORT_FORBIDDEN','EXPORT_EXPIRED','EXPORT_LIMIT_EXCEEDED','EXPORT_TOO_LARGE','EXPORT_INVALID_DATE','EXPORT_INTERRUPTED','EXPORT_DOWNLOAD_FAILED','EXPORT_STORAGE_FULL']);
export function DataExportButton({ dataset, agentId='', scope='agent' }: {dataset:ExportDataset; agentId?:string; scope?:'shared'|'agent'}) {
  const intl=useIntl();
  const t=(id:string)=>intl.formatMessage({id:`exports.${id}`});
  const [open,setOpen]=useState(false);
  const [selected,setSelected]=useState(dataset);
  const [format,setFormat]=useState<ExportFormat>('xlsx');
  const [agent,setAgent]=useState(agentId);
  const [wikiScope,setWikiScope]=useState(scope);
  const [agents,setAgents]=useState<{name:string;display_name:string}[]>([]);
  const [start,setStart]=useState(''); const [end,setEnd]=useState('');
  const [job,setJob]=useState<ExportJob|null>(null);
  const [busy,setBusy]=useState(false); const [error,setError]=useState<string|null>(null);
  const sequence=useRef(0);
  const report=(value:unknown)=>{
    const message=value instanceof Error ? value.message : typeof value === 'string' ? value : value && typeof value === 'object' && 'code' in value ? String(value.code) : value && typeof value === 'object' && 'message' in value ? String(value.message) : '';
    const code=message.match(/\bEXPORT_[A-Z_]+\b/)?.[0] ?? '';
    setError(knownErrors.has(code)?code:'generic');
  };
  useEffect(()=>{
    if(!open) return;
    let alive=true;
    api.agents.list().then(r=>{if(alive) setAgents(r?.agents??[]);}).catch(()=>{});
    return ()=>{alive=false;};
  },[open]);
  useEffect(()=>{
    if(!open || !job || !['queued','running'].includes(job.state)) return;
    let alive=true; let polling=false;
    const timer=setInterval(()=>{
      if(polling) return;
      polling=true; const run=sequence.current;
      dataExports.get(job.export_id).then(next=>{if(alive && run===sequence.current){setJob(next);setError(null);}}).catch(e=>{if(alive) report(e);}).finally(()=>{polling=false;});
    },1000);
    return ()=>{alive=false;clearInterval(timer);};
  },[open,job?.export_id,job?.state]);
  useEffect(()=>()=>{sequence.current++;},[]);
  const requiresAgent=selected==='memories' || (selected==='wiki' && wikiScope==='agent');
  const active=!!job && ['queued','running'].includes(job.state);
  const create=async()=>{
    setBusy(true);setError(null);setJob(null);const run=++sequence.current;
    try {
      const created=await dataExports.create({dataset:selected,format,...exportDateRange(start,end),...(agent && !(selected==='wiki' && wikiScope==='shared') ? {agent_id:agent}:{}),...(selected==='wiki'?{scope:wikiScope}:{})});
      if(run===sequence.current) setJob({export_id:created.export_id,state:'queued',dataset:selected,format});
      const next=await dataExports.get(created.export_id);
      if(run===sequence.current) setJob(next);
    } catch(e){if(run===sequence.current)report(e);} finally{if(run===sequence.current)setBusy(false);}
  };
  const cancel=async()=>{
    if(!job)return;sequence.current++;setBusy(true);setError(null);
    try{await dataExports.cancel(job.export_id);setJob(await dataExports.get(job.export_id));}catch(e){report(e);}finally{setBusy(false);}
  };
  const download=async()=>{
    if(!job || job.state!=='ready')return;setBusy(true);setError(null);
    try{await downloadDataExport(job.export_id,job.dataset,job.format);}catch(e){report(e);}finally{setBusy(false);}
  };
  return <>
    <Button variant="outline" size="sm" onClick={()=>{if(!job){setSelected(dataset);setAgent(agentId);setWikiScope(scope);}setOpen(true);}} aria-label={t('open')}><Download className="size-4"/><span>{t('open')}</span></Button>
    <Dialog open={open} onOpenChange={setOpen}><DialogContent className="sm:max-w-lg"><DialogHeader><DialogTitle>{t('title')}</DialogTitle></DialogHeader>
      <div className="space-y-3">
        <label className="grid gap-1 text-sm">{t('dataset')}<select className="rounded border border-surface-border bg-background p-2" value={selected} disabled={busy||active} onChange={e=>{setSelected(e.target.value as ExportDataset);setJob(null);}}>{datasets.map(d=><option key={d} value={d}>{t(`dataset.${d}`)}</option>)}</select></label>
        {selected==='wiki' && <label className="grid gap-1 text-sm">{t('scope')}<select className="rounded border border-surface-border bg-background p-2" value={wikiScope} disabled={busy||active} onChange={e=>{setWikiScope(e.target.value as 'shared'|'agent');setJob(null);}}><option value="agent">{t('scope.agent')}</option><option value="shared">{t('scope.shared')}</option></select></label>}
        {!(selected==='wiki' && wikiScope==='shared') && <label className="grid gap-1 text-sm">{t('agent')}<select className="rounded border border-surface-border bg-background p-2" value={agent} disabled={busy||active} onChange={e=>{setAgent(e.target.value);setJob(null);}}><option value="">{t(requiresAgent?'agent':'allAgents')}</option>{agents.map(a=><option key={a.name} value={a.name}>{a.display_name||a.name}</option>)}</select></label>}
        <p className="text-xs text-muted-foreground">{t(`date.${selected}`)} {t('timezone')}</p>
        <div className="grid grid-cols-2 gap-3"><label className="grid gap-1 text-sm">{t('from')}<Input type="date" value={start} disabled={busy||active} onChange={e=>{setStart(e.target.value);setJob(null);}}/></label><label className="grid gap-1 text-sm">{t('through')}<Input type="date" value={end} disabled={busy||active} onChange={e=>{setEnd(e.target.value);setJob(null);}}/></label></div>
        <label className="grid gap-1 text-sm">{t('format')}<select className="rounded border border-surface-border bg-background p-2" value={format} disabled={busy||active} onChange={e=>{setFormat(e.target.value as ExportFormat);setJob(null);}}><option value="csv">CSV (UTF-8)</option><option value="xlsx">Excel (.xlsx)</option></select></label>
        <p className="text-xs text-muted-foreground">{t('rowsNote')}</p>
        {selected==='usage' && <p className="text-xs text-muted-foreground">{t('costNote')}</p>}
        {error && <p role="alert" className="text-sm text-destructive">{t(`error.${error}`)}</p>}
        {job && <div role="status" aria-live="polite" className="rounded border border-surface-border p-3 text-sm"><p>{t(`state.${job.state}`)}</p>
          {job.state==='ready' && <><p>{intl.formatMessage({id:'exports.counts'},{logical:job.logical_rows??0,physical:job.physical_rows??0})}</p><p>{intl.formatMessage({id:'exports.expiry'},{time:job.expires_at?new Date(job.expires_at).toLocaleString(intl.locale,{timeZone:'Asia/Taipei'}):'—'})}</p></>}
          {job.state==='failed' && <p>{t(`error.${knownErrors.has(job.error_code??'')?job.error_code:'generic'}`)}</p>}
        </div>}
        <div className="flex flex-wrap justify-end gap-2">{active?<Button variant="outline" disabled={busy} onClick={()=>void cancel()}>{t('cancel')}</Button>:<Button variant="brand" disabled={busy || (requiresAgent && !agent)} onClick={()=>void create()}>{t('create')}</Button>}{job?.state==='ready' && <Button disabled={busy} onClick={()=>void download()}>{t('download')}</Button>}</div>
      </div>
    </DialogContent></Dialog>
  </>;
}
