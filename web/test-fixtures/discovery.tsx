/** Browser acceptance fixture only: all RPC replies are synthetic, zero LLM. */
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { IntlProvider } from 'react-intl';
import { DiscoverySection } from '../src/components/tasks/DiscoverySection';
import { client } from '../src/lib/ws-client';
import type { DiscoveryTree } from '../src/lib/discovery-api';
import en from '../src/i18n/en.json';
import zh from '../src/i18n/zh-TW.json';
import ja from '../src/i18n/ja-JP.json';
import '../src/index.css';
const base={usd:0,usd_source:'unknown' as const,unknown_calls:1,wall_secs:2.5,input_tokens:0,output_tokens:0,cache_read_tokens:0};
const tree:DiscoveryTree={run:{run_id:'synthetic-run',task_id:'synthetic-task',title:'Synthetic file exploration',status:'completed',approval_status:'approved',current_round:2,branch_count:2,refine_count:2,runtime:'codex',budget:{max_agent_calls:4,max_usd:1,max_wall_secs:90,max_rounds:2},cost:base,can_cancel:false,artifact_verified:true,degraded:true},nodes:[{cell_id:'r1-b0-a0',round:1,branch:0,attempt:0,parent_id:null,status:'ok',score:3.14159,model:'observed-model',cost:{...base,usd:0.00051,usd_source:'reported',unknown_calls:0}},{cell_id:'r1-b1-a0',round:1,branch:1,attempt:0,parent_id:'r1-b0-a0',status:'timeout',score:null,model:null,cost:{...base,usd:0.00125,usd_source:'estimated'}},{cell_id:'r2-b0-a0',round:2,branch:0,attempt:0,parent_id:'r1-b0-a0',status:'ok',score:3.5,model:null,cost:base}],rounds:[{round:1,policy_id:'uniform',beta:0,full_grid:['r1-b0-a0','r1-b1-a0'],completion:['r1-b0-a0','r1-b1-a0'],new_in:['r1-b0-a0','r1-b1-a0']},{round:2,policy_id:'best',beta:1,full_grid:['r2-b0-a0','r2-b1-a0'],completion:['r2-b0-a0'],new_in:['r2-b0-a0','r2-b1-a0']}]};
client.call=async(method,params={})=>{
  if(method==='discovery.catalog')return {roots:[{id:'approved-synthetic',label:'Approved synthetic files'}],evaluators:[{name:'synthetic-sum',label:'Synthetic sum evaluator'}],runtimes:['codex','claude','gemini','openai-compat'],can_create:true,requires_approval:true};
  if(method==='discovery.list')return {runs:[tree.run]};
  if(method==='discovery.tree')return tree;
  if(method==='tasks.create')return {task_id:'synthetic-only-created',run_id:'synthetic-only-created',status:'pending_approval',approval_id:'synthetic-approval'};
  if(method==='discovery.artifact'&&params.file_id)return {run_id:'synthetic-run',cell_id:'r2-b0-a0',file_id:'opaque-file',name:'synthetic-output.txt',size_bytes:2,content_base64:'b2s='};
  if(method==='discovery.artifact')return {run_id:'synthetic-run',cell_id:'r2-b0-a0',verified:true,files:[{file_id:'opaque-file',name:'synthetic-output.txt',size_bytes:2}]};
  throw new Error('Unsupported synthetic method');
};
function Fixture(){const [locale,setLocale]=useState<'en'|'zh-TW'|'ja-JP'>('en');return <IntlProvider locale={locale} messages={{en,'zh-TW':zh,'ja-JP':ja}[locale]}><main className="mx-auto max-w-5xl space-y-4 p-4"><p className="rounded-md border border-amber-500 p-2 text-sm">MOCK FIXTURE ONLY — synthetic RPCs, no backend / provider calls.</p><label>Fixture language <select value={locale} onChange={e=>setLocale(e.target.value as typeof locale)}><option value="en">English</option><option value="zh-TW">繁體中文</option><option value="ja-JP">日本語</option></select></label><DiscoverySection agentId="synthetic-employee" enabled/></main></IntlProvider>};
createRoot(document.getElementById('root')!).render(<Fixture/>);
