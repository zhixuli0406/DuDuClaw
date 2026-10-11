import { client } from './ws-client';
import { useConnectionStore } from '@/stores/connection-store';
import { describe, it, expect, vi } from 'vitest';
import { policyAllows, useProductPolicyStore, type ProductPolicy } from './product-policy';
const policy: ProductPolicy={profile:'futureoffice',blocked_channels:['wecom','dingtalk','feishu'],blocked_runtimes:['qwen','kimi'],blocked_providers:['minimax','deepseek']};
describe('product policy precise filters',()=>{
 it('blocks named runtime, but retains Qwen API and local models',()=>{
   expect(policyAllows(policy,'runtimes','qwen')).toBe(false);
   expect(policyAllows(policy,'providers','qwen')).toBe(true);
   expect(policyAllows(policy,'runtimes','local:qwen3')).toBe(true);
   expect(policyAllows(policy,'channels','feishu')).toBe(false);
 });
 it('fails closed during unknown policy without blocking allowed items',()=>{
   expect(policyAllows(null,'providers','deepseek')).toBe(false);
   expect(policyAllows(null,'providers','anthropic')).toBe(true);
   expect(policyAllows({profile:'standard',blocked_channels:[],blocked_runtimes:[],blocked_providers:[]},'runtimes','kimi')).toBe(true);
 });
});

describe('product policy connection lifetime',()=>{
 it('clears a standard policy on disconnect and fails closed when reload fails',async()=>{
   useProductPolicyStore.setState({policy:{profile:'standard',blocked_channels:[],blocked_runtimes:[],blocked_providers:[]},loading:false,error:false});
   useConnectionStore.setState({state:'connected'});
   expect(useProductPolicyStore.getState().policy).toBeNull();
   const call=vi.spyOn(client,'call').mockRejectedValue(new Error('offline'));
   await useProductPolicyStore.getState().load();
   expect(useProductPolicyStore.getState().error).toBe(true);
   expect(policyAllows(useProductPolicyStore.getState().policy,'providers','deepseek')).toBe(false);
   call.mockRestore();
 });
 it('ignores a delayed policy response from an earlier connection',async()=>{
   let resolve!: (value:unknown)=>void;
   const call=vi.spyOn(client,'call').mockImplementation(()=>new Promise(r=>{resolve=r;}));
   useProductPolicyStore.setState({policy:null,loading:false,error:false});
   const pending=useProductPolicyStore.getState().load();
   useConnectionStore.setState({state:'disconnected'});
   resolve({profile:'standard',blocked_channels:[],blocked_runtimes:[],blocked_providers:[]});
   await pending;
   expect(useProductPolicyStore.getState().policy).toBeNull();
   call.mockRestore();
 });
});
