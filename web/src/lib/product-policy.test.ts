import { client } from './ws-client';
import { useConnectionStore } from '@/stores/connection-store';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { policyAllows, useProductPolicyStore, type ProductPolicy } from './product-policy';
const policy: ProductPolicy={profile:'restricted',blocked_channels:['slack'],blocked_runtimes:['claude'],blocked_providers:['openai']};
afterEach(()=>vi.restoreAllMocks());
describe('product policy precise filters',()=>{
 it('blocks only gateway-provided identifiers within their matching category',()=>{
   expect(policyAllows(policy,'runtimes','claude')).toBe(false);
   expect(policyAllows(policy,'providers','claude')).toBe(true);
   expect(policyAllows(policy,'runtimes','local:example-model')).toBe(true);
   expect(policyAllows(policy,'channels','slack')).toBe(false);
 });
 it('fails closed for every picker while the installation policy is unknown',()=>{
   expect(policyAllows(null,'providers','openai')).toBe(false);
   expect(policyAllows(null,'providers','anthropic')).toBe(false);
   expect(policyAllows(null,'runtimes','claude')).toBe(false);
   expect(policyAllows(null,'channels','line')).toBe(false);
   expect(policyAllows({profile:'standard',blocked_channels:[],blocked_runtimes:[],blocked_providers:[]},'runtimes','claude')).toBe(true);
 });
});

describe('product policy response validation',()=>{
 it.each(['standard','restricted'] as const)('accepts a generic %s profile',async(profile)=>{
   vi.spyOn(client,'call').mockResolvedValue({...policy,profile});
   useProductPolicyStore.setState({policy:null,loading:false,error:false});
   await useProductPolicyStore.getState().load();
   expect(useProductPolicyStore.getState().policy?.profile).toBe(profile);
   expect(useProductPolicyStore.getState().error).toBe(false);
 });
 it.each([
   {...policy,profile:'custom'},
   {...policy,blocked_channels:['slack',42]},
   {...policy,blocked_runtimes:null},
   {profile:'restricted'},
 ])('rejects an unsupported profile or malformed gateway policy',async(response)=>{
   vi.spyOn(client,'call').mockResolvedValue(response);
   useProductPolicyStore.setState({policy:null,loading:false,error:false});
   await useProductPolicyStore.getState().load();
   expect(useProductPolicyStore.getState().policy).toBeNull();
   expect(useProductPolicyStore.getState().error).toBe(true);
   expect(policyAllows(useProductPolicyStore.getState().policy,'channels','line')).toBe(false);
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
   expect(policyAllows(useProductPolicyStore.getState().policy,'providers','openai')).toBe(false);
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
