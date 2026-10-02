import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { discoveryApi, type CreateDiscoveryInput } from './discovery-api';

const input: CreateDiscoveryInput = { assigned_to: 'agnes', title: 'Explore', description: 'Synthetic task', discovery: { approved_root_id: 'approved-1', evaluator: 'sum', runtime: 'codex', model: 'model', branch_count: 2, refine_count: 1, max_parallelism: 1, budget: { max_agent_calls: 4, max_usd: 1, max_wall_secs: 90, max_rounds: 2 } } };
beforeEach(() => { vi.clearAllMocks(); mockWsClient.call.mockResolvedValue({}); });
describe('discoveryApi', () => {
  it('creates a discovery task through the common entry and drops operator/host/backend overrides', async () => {
    await discoveryApi.create({ ...input, operator: true, discovery: { ...input.discovery, starting_workspace: '/private/home', sandbox: 'none' } } as CreateDiscoveryInput);
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.create', { ...input, kind: 'discovery' });
  });
  it('scopes catalog/list/tree/cancel to approved opaque IDs', async () => {
    await discoveryApi.catalog('agnes'); await discoveryApi.list('agnes'); await discoveryApi.tree('run-1'); await discoveryApi.cancel('run-1');
    expect(mockWsClient.call.mock.calls).toEqual([
      ['discovery.catalog', { agent_id: 'agnes' }], ['discovery.list', { agent_id: 'agnes', limit: 20 }], ['discovery.tree', { run_id: 'run-1' }], ['discovery.cancel', { run_id: 'run-1' }],
    ]);
  });
});

import { artifactBlob, runtimeLabel } from './discovery-api';
describe('runtimeLabel',()=>{
  it('maps the six known runtime ids to product names',()=>{
    expect(['claude','codex','gemini','antigravity','grok','openai-compat'].map(id=>runtimeLabel(id))).toEqual(['Claude','Codex','Gemini','Antigravity','Grok','OpenAI-compatible']);
  });
  it('marks a deprecated runtime (gemini) with the localized marker only when intl is given',()=>{
    const intl={messages:{},formatMessage:({id}:{id:string})=>id==='common.deprecated'?'已棄用':id};
    expect(runtimeLabel('gemini',intl)).toBe('Gemini (已棄用)');
    expect(runtimeLabel('gemini')).toBe('Gemini');
    expect(runtimeLabel('antigravity',intl)).toBe('Antigravity');
  });
  it('passes unknown ids through and never returns empty for a non-empty id',()=>{
    expect(runtimeLabel('brand-new')).toBe('brand-new');
    expect(runtimeLabel('constructor')).toBe('constructor');
  });
});
describe('verified artifact payload boundary',()=>{
  it('accepts an exact bounded artifact response and rejects foreign IDs, host filenames and size mismatch',()=>{
    const payload={run_id:'run-1',cell_id:'r1-b0-a0',file_id:'opaque-1',name:'output.txt',size_bytes:2,content_base64:btoa('ok')};
    expect(artifactBlob(payload,'run-1','opaque-1').size).toBe(2);
    expect(()=>artifactBlob({...payload,run_id:'foreign'},'run-1','opaque-1')).toThrow();
    expect(()=>artifactBlob({...payload,name:'/private/home/file'},'run-1','opaque-1')).toThrow();
    expect(()=>artifactBlob({...payload,size_bytes:3},'run-1','opaque-1')).toThrow();
  });
});
