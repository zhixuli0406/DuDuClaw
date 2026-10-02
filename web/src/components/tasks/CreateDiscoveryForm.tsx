import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Button, Input } from '@/components/mds';
import { runtimeLabel, type DiscoveryCatalog, type CreateDiscoveryInput } from '@/lib/discovery-api';

// Number inputs keep their raw text: an empty field must stay invalid instead of
// collapsing to `Number('') === 0` (0 is a legal refine_count).
const parseField=(raw:string)=>raw.trim()===''?Number.NaN:Number(raw);

export function CreateDiscoveryForm({catalog,agentId,onCreate}:{catalog:DiscoveryCatalog;agentId:string;onCreate:(input:CreateDiscoveryInput)=>Promise<void>}) {
  const intl=useIntl();const t=(key:string)=>intl.formatMessage({id:`discovery.${key}`});
  const [title,setTitle]=useState('');const [description,setDescription]=useState('');const [model,setModel]=useState('');
  const [root,setRoot]=useState(catalog.roots[0]?.id??'');const [evaluator,setEvaluator]=useState(catalog.evaluators[0]?.name??'');const [runtime,setRuntime]=useState(catalog.runtimes[0]??'');
  const [fields,setFields]=useState<Record<string,string>>({max_agent_calls:'4',max_usd:'1',max_wall_secs:'90',max_rounds:'2',branch_count:'2',refine_count:'1',max_parallelism:'1'});
  const numbers={max_agent_calls:parseField(fields.max_agent_calls),max_usd:parseField(fields.max_usd),max_wall_secs:parseField(fields.max_wall_secs),max_rounds:parseField(fields.max_rounds),branch_count:parseField(fields.branch_count),refine_count:parseField(fields.refine_count),max_parallelism:parseField(fields.max_parallelism)};
  const numberOk=(key:string,value:number)=>Number.isFinite(value)&&(key==='refine_count'?value>=0:value>0)&&(key==='max_usd'||Number.isSafeInteger(value));
  const [busy,setBusy]=useState(false);const [error,setError]=useState(false);
  if(!catalog.can_create) return <p className="text-sm text-muted-foreground">{t('notPermitted')}</p>;
  const valid=!!agentId && title.trim().length>0 && description.trim().length>0 && model.trim().length>0
    && catalog.roots.some(v=>v.id===root) && catalog.evaluators.some(v=>v.name===evaluator) && catalog.runtimes.includes(runtime)
    && Object.entries(numbers).every(([key,value])=>numberOk(key,value));
  const nodesKnown=numberOk('branch_count',numbers.branch_count)&&numberOk('refine_count',numbers.refine_count);
  const select=(id:string,value:string,change:(v:string)=>void,options:{value:string;label:string}[])=> <label className="grid gap-1 text-xs font-medium">{t(id)}<select className="h-9 min-w-0 rounded-md border border-surface-border bg-surface-raised px-2 text-sm" value={value} onChange={e=>change(e.target.value)} required>{options.map(option=><option key={option.value} value={option.value}>{option.label}</option>)}</select></label>;
  return <form className="space-y-3" onSubmit={async e=>{e.preventDefault();if(!valid||busy)return;setBusy(true);setError(false);try{await onCreate({assigned_to:agentId,title:title.trim(),description:description.trim(),discovery:{approved_root_id:root,evaluator,runtime,model:model.trim(),branch_count:numbers.branch_count,refine_count:numbers.refine_count,max_parallelism:numbers.max_parallelism,budget:{max_agent_calls:numbers.max_agent_calls,max_usd:numbers.max_usd,max_wall_secs:numbers.max_wall_secs,max_rounds:numbers.max_rounds}}});}catch{setError(true);}finally{setBusy(false);}}}>
    {catalog.requires_approval && <p role="status" className="text-sm text-amber-700 dark:text-amber-300">{t('requiresApproval')}</p>}
    <label className="grid gap-1 text-xs font-medium">{t('titleField')}<Input value={title} onChange={e=>setTitle(e.target.value)} maxLength={200} required/></label>
    <label className="grid gap-1 text-xs font-medium">{t('descriptionField')}<textarea className="min-h-20 rounded-md border border-surface-border bg-surface-raised p-2 text-sm" value={description} onChange={e=>setDescription(e.target.value)} maxLength={8000} required/></label>
    <div className="grid gap-3 sm:grid-cols-2">{select('root',root,setRoot,catalog.roots.map(v=>({value:v.id,label:v.label})))}{select('evaluator',evaluator,setEvaluator,catalog.evaluators.map(v=>({value:v.name,label:v.label})))}{select('runtime',runtime,setRuntime,catalog.runtimes.map(v=>({value:v,label:runtimeLabel(v,intl)})))}<label className="grid gap-1 text-xs font-medium">{t('model')}<Input value={model} onChange={e=>setModel(e.target.value)} maxLength={200} required/></label></div>
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">{Object.entries(fields).map(([key,value])=><label key={key} className="grid gap-1 text-xs font-medium">{t(`budget.${key}`)}<Input type="number" min={key==='max_usd'?0.000001:key==='refine_count'?0:1} step={key==='max_usd'?'any':1} value={value} onChange={e=>setFields(prev=>({...prev,[key]:e.target.value}))} required/></label>)}</div>
    <p className="text-xs text-muted-foreground">{t('refineHelp')}{nodesKnown && <> {intl.formatMessage({id:'discovery.refineHelpNodes'},{nodes:intl.formatNumber(numbers.branch_count*(numbers.refine_count+1))})}</>}</p>
    <p className="text-xs text-muted-foreground">{t('budgetNotice')}</p>
    {error && <p role="alert" className="text-sm text-destructive">{t('createError')}</p>}
    <Button type="submit" variant="brand" disabled={!valid||busy}>{t('create')}</Button>
  </form>;
}
