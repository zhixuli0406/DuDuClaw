import { useState } from 'react';
import { useIntl } from 'react-intl';
import { GitBranch, Download, Square } from 'lucide-react';
import { Button, Card, CardContent } from '@/components/mds';
import { runtimeLabel, type DiscoveryCost, type DiscoveryTree } from '@/lib/discovery-api';

function CostLabel({ cost }: { cost: DiscoveryCost }) {
  const intl=useIntl();
  const known=Number.isFinite(cost.usd) && cost.usd>=0 ? intl.formatNumber(cost.usd, {style:'currency',currency:'USD',minimumFractionDigits:2,maximumFractionDigits:6}) : null;
  const source=['reported','estimated','unknown','pending'].includes(cost.usd_source) ? cost.usd_source : 'unknown';
  return <span>{intl.formatMessage({id:`discovery.cost.${source}`}, {amount:known??'—'})}{source==='unknown' && cost.usd>0 ? ` · ${intl.formatMessage({id:'discovery.cost.subtotal'}, {amount:known})}` : ''}</span>;
}
export function DiscoveryTreeCard({ tree, onCancel, onDownload }: { tree: DiscoveryTree; onCancel: () => Promise<void>; onDownload: () => Promise<void> }) {
  const intl=useIntl();const [busy,setBusy]=useState(false);const [error,setError]=useState(false);
  const message=(id:string,values?:Record<string,string|number>)=>intl.formatMessage({id:`discovery.${id}`},values);
  // Known enum values are localized; unknown ones fall back to the raw value (never an ICU-parsed default).
  const label=(group:string,value:string|null|undefined,fallback?:string)=>value&&`discovery.${group}.${value}` in intl.messages?intl.formatMessage({id:`discovery.${group}.${value}`}):fallback??value??'—';
  const run=tree.run;const approval=run.approval_status==='pending_approval'?'pending':run.approval_status;
  const stop=run.stop_code?label('stop',run.stop_code,message('stop.other')):null;
  const cancelReason=run.cancel_code?label('cancelReason',run.cancel_code):null;
  const expires=run.approval_expires_at?new Date(run.approval_expires_at):null;
  const expiresText=expires&&Number.isFinite(expires.getTime())?intl.formatDate(expires,{dateStyle:'medium',timeStyle:'short'}):null;
  const act=async (action:()=>Promise<void>)=>{if(busy)return;setBusy(true);setError(false);try{await action();}catch{setError(true);}finally{setBusy(false);}};
  return <Card data-size="sm"><CardContent className="space-y-3 py-4">
    <div className="flex flex-wrap items-start justify-between gap-2">
      <div className="min-w-0"><h3 className="flex items-center gap-2 text-sm font-medium"><GitBranch className="size-4 shrink-0" aria-hidden="true"/><span className="break-words">{tree.run.title}</span></h3>
        <p className="mt-1 text-xs text-muted-foreground">{runtimeLabel(tree.run.runtime,intl)} · {message('status',{status:label('runStatus',run.status)})} · {message('approval',{status:label('approvalStatus',approval)})}</p></div>
      <div className="flex flex-wrap gap-2">{tree.run.can_cancel && <Button variant="outline" size="sm" disabled={busy} onClick={()=>void act(onCancel)}><Square className="size-3"/>{message('cancel')}</Button>}
        {tree.run.artifact_verified && <Button variant="outline" size="sm" disabled={busy} onClick={()=>void act(onDownload)}><Download className="size-3"/>{message('download')}</Button>}</div>
    </div>
    {run.isolation_degraded===true && <p className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">{message('degraded')}</p>}
    {cancelReason && <p className="text-xs text-muted-foreground">{cancelReason}</p>}
    {stop && <p role="status" className="rounded-md border border-surface-border px-3 py-2 text-xs text-foreground">{stop}</p>}
    {approval==='pending' && <p role="status" className="text-xs text-muted-foreground">{expiresText?message('approvalHintExpires',{time:expiresText}):message('approvalHint')}</p>}
    <p className="text-xs text-muted-foreground"><CostLabel cost={tree.run.cost}/> · {message('wall',{seconds:Number.isFinite(run.cost.wall_secs)?intl.formatNumber(run.cost.wall_secs,{maximumFractionDigits:1}):'—'})}</p>
    {error && <p role="alert" className="text-xs text-destructive">{message('actionError')}</p>}
    {tree.run.tree_available===false?<p role="status" className="text-xs text-muted-foreground">{message('treeUnavailable')}</p>:<>{tree.rounds.map(round=><details key={round.round} open className="rounded-md border border-surface-border p-3">
      <summary className="cursor-pointer text-xs font-medium">{message('round',{round:round.round})} · {message('planned',{cells:round.full_grid.length})} · {message('completedCount',{done:round.completion.length,total:round.full_grid.length})}</summary>
      <div className="mt-2 space-y-1 break-words font-mono text-xs text-muted-foreground"><p>{message('grid',{cells:round.full_grid.join(', ')||'—'})}</p><p>{message('completed',{cells:round.completion.join(', ')||'—'})}</p></div>
    </details>)}
    <div className="overflow-x-auto"><table className="w-full text-left text-xs"><caption className="sr-only">{message('nodes')}</caption><thead><tr className="border-b border-surface-border">{['cell','parent','nodeStatus','score','model','costLabel'].map(key=><th key={key} scope="col" className="whitespace-nowrap p-2 font-medium">{message(key)}</th>)}</tr></thead>
      <tbody>{tree.nodes.map(node=><tr key={node.cell_id} className="border-b border-surface-border last:border-0"><th scope="row" className="whitespace-nowrap p-2 font-mono font-normal">{node.cell_id}</th><td className="whitespace-nowrap p-2 font-mono">{node.parent_id ? `${node.parent_id} → ${node.cell_id}` : '—'}</td><td className="p-2">{label('node',node.status)}</td><td className="p-2 font-mono tabular-nums">{node.score!==null && Number.isFinite(node.score) ? intl.formatNumber(node.score,{maximumFractionDigits:6}) : '—'}</td><td className="p-2">{node.model||message('modelMissing')}</td><td className="whitespace-nowrap p-2"><CostLabel cost={node.cost}/></td></tr>)}</tbody></table></div>
    {tree.nodes.length===0 && <p className="text-xs text-muted-foreground">{message('noNodes')}</p>}
    </>}
  </CardContent></Card>;
}
