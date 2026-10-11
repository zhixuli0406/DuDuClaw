import { useEffect } from 'react';
import { create } from 'zustand';
import { client } from './ws-client';
import { useConnectionStore } from '@/stores/connection-store';
export interface ProductPolicy {
  profile: string; blocked_channels: string[]; blocked_runtimes: string[]; blocked_providers: string[];
}
export type PolicyKind = 'channels' | 'runtimes' | 'providers';
const restricted: Record<PolicyKind, readonly string[]> = {channels:['wecom','dingtalk','feishu'],runtimes:['qwen','kimi'],providers:['minimax','deepseek']};
export function policyAllows(policy: ProductPolicy | null, kind: PolicyKind, id: string): boolean {
  return !(policy ? policy[`blocked_${kind}`] : restricted[kind]).includes(id);
}
let generation = 0;
interface PolicyStore { policy: ProductPolicy | null; loading: boolean; error: boolean; load: () => Promise<void>; }
export const useProductPolicyStore = create<PolicyStore>((set, get) => ({
  policy:null,loading:false,error:false,
  load:async()=>{
    if(get().loading) return;
    const current = generation;
    set({policy:null,loading:true,error:false});
    try {
      const result=await client.call('system.product_profile') as ProductPolicy;
      if(!result || !['standard','futureoffice'].includes(result.profile) || !['channels','runtimes','providers'].every(k=>{ const values=result[`blocked_${k}` as keyof ProductPolicy]; return Array.isArray(values) && values.every(value=>typeof value==='string'); })) throw new Error('INVALID_POLICY');
      if(current === generation) set({policy:result,loading:false,error:false});
    } catch { if(current === generation) set({policy:null,loading:false,error:true}); }
  },
}));
// Discard installation policy at every transport transition, including logout and reconnect.
useConnectionStore.subscribe((state, previous) => {
  if(state.state === previous.state) return;
  generation++;
  useProductPolicyStore.setState({policy:null,loading:false,error:false});
  if(state.state === 'authenticated') void useProductPolicyStore.getState().load();
});
export function useProductPolicy() {
  const store=useProductPolicyStore();
  const connection=useConnectionStore(s=>s.state);
  useEffect(()=>{ if(connection==='authenticated' && !store.policy && !store.loading && !store.error) void store.load(); },[connection,store.policy,store.loading,store.error,store.load]);
  return { ...store, allows:(kind:PolicyKind,id:string)=>policyAllows(store.policy,kind,id) };
}
