import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { DiscoverySection } from './DiscoverySection';
import type { DiscoveryTree } from '@/lib/discovery-api';
const tree:DiscoveryTree={run:{run_id:'run-1',task_id:'task-1',title:'Verified tree',status:'running',approval_status:'pending_approval',current_round:1,branch_count:1,refine_count:1,runtime:'codex',budget:{max_agent_calls:2,max_usd:1,max_wall_secs:30,max_rounds:1},cost:{usd:0,usd_source:'unknown',unknown_calls:1,wall_secs:1,input_tokens:0,output_tokens:0,cache_read_tokens:0},can_cancel:false,artifact_verified:false,degraded:false},nodes:[],rounds:[]};
beforeEach(()=>{vi.clearAllMocks();mockWsClient.call.mockImplementation((method:string)=>Promise.resolve(method==='discovery.list'?{runs:[tree.run]}:method==='discovery.tree'?tree:method==='discovery.catalog'?{can_create:false,requires_approval:true,roots:[],evaluators:[],runtimes:[]}:{}));});
afterEach(()=>vi.useRealTimers());
describe('DiscoverySection',()=>{
  it('keeps normal and oversized summaries visible with costs, cancellation and verified artifacts',async()=>{
    const legacy={...tree.run,run_id:'legacy',task_id:'legacy-task',title:'Oversized historical exploration',
      tree_available:false,tree_unavailable_reason:'discovery tree exceeds the bounded query size',
      can_cancel:true,artifact_verified:true,cost:{...tree.run.cost,usd:0.125,usd_source:'reported' as const}};
    mockWsClient.call.mockImplementation((method:string,params:{run_id?:string;file_id?:string})=>{
      if(method==='discovery.list')return Promise.resolve({runs:[tree.run,legacy]});
      if(method==='discovery.tree')return params.run_id==='legacy'?Promise.reject(new Error('bounded query size')):Promise.resolve(tree);
      if(method==='discovery.artifact')return Promise.resolve({run_id:'legacy',cell_id:'cell-1',verified:true,files:[{file_id:'opaque',name:'legacy-result.txt',size_bytes:2}]});
      return Promise.resolve({});
    });
    renderWithProviders(<DiscoverySection agentId="agnes" enabled/>);
    expect(await screen.findByText('Verified tree')).toBeInTheDocument();
    expect(screen.getByText('Oversized historical exploration')).toBeInTheDocument();
    expect(screen.getByText(/\$0\.125/)).toBeInTheDocument();
    expect(screen.getByText(/The tree could not be loaded/)).toBeInTheDocument();
    expect(screen.queryByText('discovery tree exceeds the bounded query size')).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('discovery.tree',{run_id:'legacy'});
    expect(screen.getAllByText('No evaluated nodes yet.')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button',{name:'Cancel exploration'}));
    await waitFor(()=>expect(mockWsClient.call).toHaveBeenCalledWith('discovery.cancel',{run_id:'legacy'}));
    fireEvent.click(await screen.findByRole('button',{name:'Download verified files'}));
    expect(await screen.findByRole('button',{name:'legacy-result.txt'})).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('discovery.artifact',{run_id:'legacy'});
  });
  it('keeps other trees and a run summary when one tree request fails',async()=>{
    const failed={...tree.run,run_id:'failed-tree',title:'Temporarily unavailable tree',can_cancel:true};
    mockWsClient.call.mockImplementation((method:string,params:{run_id?:string})=>{
      if(method==='discovery.list')return Promise.resolve({runs:[tree.run,failed]});
      if(method==='discovery.tree')return params.run_id==='failed-tree'?Promise.reject(new Error('temporary failure')):Promise.resolve(tree);
      return Promise.resolve({});
    });
    renderWithProviders(<DiscoverySection agentId="agnes" enabled/>);
    expect(await screen.findByText('Verified tree')).toBeInTheDocument();
    expect(screen.getByText('Temporarily unavailable tree')).toBeInTheDocument();
    expect(screen.getByText(/The tree could not be loaded/)).toBeInTheDocument();
    expect(screen.getByRole('button',{name:'Cancel exploration'})).toBeEnabled();
    expect(screen.getAllByText('No evaluated nodes yet.')).toHaveLength(1);
  });
  it('renders approval status independently of catalog permission and does not show a create button',async()=>{
    renderWithProviders(<DiscoverySection agentId="agnes" enabled/>);
    expect(await screen.findByText('Verified tree')).toBeInTheDocument();
    expect(screen.getByText(/Awaiting manager approval/)).toBeInTheDocument();
    expect(screen.queryByRole('button',{name:'Create exploration'})).not.toBeInTheDocument();
  });
  it('does not send stale private trees into a different employee scope',async()=>{
    let resolveOld!:(value:unknown)=>void;
    mockWsClient.call.mockImplementation((method:string,params:{agent_id?:string})=>method==='discovery.list'&&params.agent_id==='old'?new Promise(resolve=>{resolveOld=resolve;}):Promise.resolve(method==='discovery.list'?{runs:[]}:method==='discovery.tree'?tree:{}));
    const view=renderWithProviders(<DiscoverySection agentId="old" enabled/>);
    await waitFor(()=>expect(resolveOld).toBeDefined());view.rerender(<DiscoverySection agentId="new" enabled/>);
    await act(async()=>resolveOld({runs:[tree.run]}));
    expect(screen.queryByText('Verified tree')).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('discovery.tree',{run_id:'run-1'});
  });
  it('cleans up poll timers and never polls after unmount',async()=>{
    vi.useFakeTimers();const view=renderWithProviders(<DiscoverySection agentId="agnes" enabled/>);
    await act(async()=>{await Promise.resolve();await Promise.resolve();await Promise.resolve();});
    expect(mockWsClient.call).toHaveBeenCalledWith('discovery.tree',{run_id:'run-1'});
    view.unmount();const count=mockWsClient.call.mock.calls.length;
    await act(async()=>vi.advanceTimersByTimeAsync(60000));expect(mockWsClient.call.mock.calls.length).toBe(count);
  });
  it.each(['complete','degraded'])('does not poll persisted terminal status %s',async(status)=>{
    vi.useFakeTimers();
    mockWsClient.call.mockImplementation((method:string)=>Promise.resolve(method==='discovery.list'?{runs:[tree.run]}:method==='discovery.tree'?{...tree,run:{...tree.run,status}}:{}));
    const view=renderWithProviders(<DiscoverySection agentId="agnes" enabled/>);
    await act(async()=>{await Promise.resolve();await Promise.resolve();await Promise.resolve();});
    expect(mockWsClient.call).toHaveBeenCalledWith('discovery.tree',{run_id:'run-1'});
    const count=mockWsClient.call.mock.calls.length;
    await act(async()=>vi.advanceTimersByTimeAsync(30000));
    expect(mockWsClient.call.mock.calls.length).toBe(count);view.unmount();
  });
  it('allows a new scope to download while a stale scope download remains pending',async()=>{
    let resolveOld!:(value:unknown)=>void;
    mockWsClient.call.mockImplementation((method:string,params:{run_id?:string;file_id?:string})=>{
      if(method==='discovery.artifact'&&params.file_id)return new Promise(resolve=>{resolveOld=resolve;});
      return Promise.resolve(method==='discovery.list'?{runs:[tree.run]}:method==='discovery.tree'?{...tree,run:{...tree.run,artifact_verified:true}}:method==='discovery.artifact'?{run_id:'run-1',cell_id:'cell-1',verified:true,files:[{file_id:'opaque',name:'result.txt',size_bytes:2}]}:{});
    });
    const view=renderWithProviders(<DiscoverySection agentId="old" enabled/>);
    fireEvent.click(await screen.findByRole('button',{name:'Download verified files'}));
    fireEvent.click(await screen.findByRole('button',{name:'result.txt'}));
    await waitFor(()=>expect(resolveOld).toBeDefined());
    expect(screen.getByRole('button',{name:'result.txt'})).toBeDisabled();
    view.rerender(<DiscoverySection agentId="new" enabled/>);
    fireEvent.click(await screen.findByRole('button',{name:'Download verified files'}));
    expect(await screen.findByRole('button',{name:'result.txt'})).toBeEnabled();
    await act(async()=>resolveOld({run_id:'run-1',cell_id:'cell-1',file_id:'opaque',name:'result.txt',size_bytes:2,content_base64:'b2s='}));
    expect(screen.getByRole('button',{name:'result.txt'})).toBeEnabled();
  });

});
