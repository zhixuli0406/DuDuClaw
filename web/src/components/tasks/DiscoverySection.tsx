import { useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { GitBranch, Plus, RefreshCw } from 'lucide-react';
import { Button, Card, CardContent, Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, Spinner } from '@/components/mds';
import { isNewFeature } from '@/apps/registry';
import { useSystemStore } from '@/stores/system-store';
import { DISCOVERY_FEATURE } from '@/lib/discovery-feature';
import { artifactBlob, discoveryApi, type DiscoveryArtifact, type DiscoveryCatalog, type DiscoveryTree } from '@/lib/discovery-api';
import { DiscoveryTreeCard } from './DiscoveryTreeCard';
import { CreateDiscoveryForm } from './CreateDiscoveryForm';

const TERMINAL = new Set(['done','completed','complete','degraded','cancelled','failed','interrupted','budget_exhausted','rate_limited']);
/** At most one serialized polling cycle; ignore stale responses after scope changes. */
export function DiscoverySection({ agentId, enabled }: { agentId?: string; enabled: boolean }) {
  const intl=useIntl();const t=(key:string)=>intl.formatMessage({id:`discovery.${key}`});
  const version=useSystemStore(state=>state.status?.version??null);
  const [trees,setTrees]=useState<DiscoveryTree[]>([]);const [catalog,setCatalog]=useState<DiscoveryCatalog|null>(null);
  const [loading,setLoading]=useState(true);const [error,setError]=useState(false);const [revision,setRevision]=useState(0);const [creating,setCreating]=useState(false);
  const [files,setFiles]=useState<DiscoveryArtifact|null>(null);const [downloadBusy,setDownloadBusy]=useState(false);const [downloadError,setDownloadError]=useState(false);
  const generation=useRef(0);const urls=useRef(new Set<string>());const cleanupTimers=useRef(new Set<ReturnType<typeof setTimeout>>());
  useEffect(()=>()=>{for(const timer of cleanupTimers.current)clearTimeout(timer);for(const url of urls.current)URL.revokeObjectURL(url);},[]);
  useEffect(()=>{
    let alive=true;let timer:ReturnType<typeof setTimeout>|undefined;let busy=false;let failures=0;
    const mine=++generation.current;
    setTrees([]);setCatalog(null);setFiles(null);setDownloadBusy(false);setDownloadError(false);setCreating(false);setLoading(enabled);setError(false);
    if(!enabled)return;
    if(agentId)void discoveryApi.catalog(agentId).then(result=>{if(alive&&generation.current===mine)setCatalog(result);}).catch(()=>{/* No catalog grants: readable runs/approval remain independent. */});
    const load=async()=>{
      if(!alive||busy||document.visibilityState==='hidden')return;
      busy=true;
      try {
        const result=await discoveryApi.list(agentId);
        if(!alive||generation.current!==mine)return;
        let treeRequestFailed=false;
        const entries=await Promise.all((result.runs??[]).slice(0,20).map(async run=>{
          if(run.tree_available===false)return {run,nodes:[],rounds:[]};
          try{return await discoveryApi.tree(run.run_id);}
          catch{
            treeRequestFailed=true;
            return {run:{...run,tree_available:false},nodes:[],rounds:[]};
          }
        }));
        if(!alive||generation.current!==mine)return;
        setTrees(entries);setError(false);failures=treeRequestFailed?failures+1:0;
        if(entries.some(tree=>!TERMINAL.has(tree.run.status))||(treeRequestFailed&&failures<3))timer=setTimeout(()=>void load(),10000);
      }catch{
        if(!alive||generation.current!==mine)return;
        setError(true);failures++;
        if(failures<3)timer=setTimeout(()=>void load(),10000*failures);
      }finally{busy=false;if(alive&&generation.current===mine)setLoading(false);}
    };
    const visible=()=>{if(document.visibilityState==='visible'){if(timer)clearTimeout(timer);void load();}};
    document.addEventListener('visibilitychange',visible);void load();
    return()=>{alive=false;generation.current++;if(timer)clearTimeout(timer);document.removeEventListener('visibilitychange',visible);};
  },[agentId,enabled,revision]);
  const refresh=()=>setRevision(value=>value+1);
  const revealFiles=async(runId:string)=>{
    const mine=generation.current;const result=await discoveryApi.artifact(runId);
    if(result.verified!==true||result.run_id!==runId||!Array.isArray(result.files))throw new Error('Artifact is not verified');
    if(mine===generation.current)setFiles(result);
  };
  const download=async(fileId:string)=>{
    if(!files||downloadBusy)return;setDownloadBusy(true);setDownloadError(false);const mine=generation.current;
    try {
      const result=await discoveryApi.download(files.run_id,fileId);
      const blob=artifactBlob(result,files.run_id,fileId);
      if(mine!==generation.current)return;
      const url=URL.createObjectURL(blob);urls.current.add(url);
      const anchor=document.createElement('a');anchor.href=url;anchor.download=result.name;document.body.appendChild(anchor);anchor.click();anchor.remove();
      const timer=setTimeout(()=>{URL.revokeObjectURL(url);urls.current.delete(url);cleanupTimers.current.delete(timer);},1000);cleanupTimers.current.add(timer);
    }catch{if(mine===generation.current)setDownloadError(true);}finally{if(mine===generation.current)setDownloadBusy(false);}
  };
  return <section aria-labelledby="discovery-heading" className="space-y-3">
    <div className="flex flex-wrap items-center justify-between gap-2"><h2 id="discovery-heading" className="flex items-center gap-2 text-sm font-medium"><GitBranch className="size-4" aria-hidden="true"/>{t('section')}{isNewFeature(DISCOVERY_FEATURE.newIn,version)&&<span className="rounded border border-surface-border px-1.5 py-0.5 text-[10px] text-muted-foreground">{t('new')}</span>}</h2><div className="flex gap-2"><Button variant="outline" size="sm" disabled={!enabled||loading} aria-label={t('refresh')} onClick={refresh}><RefreshCw className="size-3.5"/></Button>{agentId&&catalog?.can_create&&<Button variant="outline" size="sm" onClick={()=>setCreating(true)}><Plus className="size-3.5"/>{t('create')}</Button>}</div></div>
    {loading?<div role="status" className="flex justify-center py-4"><Spinner/></div>:error?<div role="alert" className="rounded-md border border-surface-border p-3 text-sm"><p>{t('loadError')}</p><Button variant="outline" size="sm" className="mt-2" onClick={refresh}>{t('retry')}</Button></div>:trees.length===0?<p className="text-xs text-muted-foreground">{agentId?t('empty'):t('selectAgent')}</p>:<div className="grid gap-3">{trees.map(tree=><DiscoveryTreeCard key={tree.run.run_id} tree={tree} onCancel={async()=>{await discoveryApi.cancel(tree.run.run_id);refresh();}} onDownload={()=>revealFiles(tree.run.run_id)}/>)}</div>}
    <Dialog open={creating} onOpenChange={setCreating}><DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-xl"><DialogHeader><DialogTitle>{t('create')}</DialogTitle><DialogDescription>{t('budgetNotice')}</DialogDescription></DialogHeader>{catalog&&agentId&&<CreateDiscoveryForm key={agentId} catalog={catalog} agentId={agentId} onCreate={async input=>{await discoveryApi.create(input);setCreating(false);refresh();}}/>}</DialogContent></Dialog>
    <Dialog open={files!==null} onOpenChange={open=>{if(!open){setFiles(null);setDownloadError(false);}}}><DialogContent><DialogHeader><DialogTitle>{t('download')}</DialogTitle></DialogHeader><Card><CardContent className="space-y-2 py-3">{files?.files.map(file=><Button key={file.file_id} variant="outline" size="sm" className="h-auto w-full justify-start break-all py-2 text-left" disabled={downloadBusy} onClick={()=>void download(file.file_id)}>{file.name}</Button>)}{downloadError&&<p role="alert" className="text-xs text-destructive">{t('actionError')}</p>}</CardContent></Card></DialogContent></Dialog>
  </section>;
}
