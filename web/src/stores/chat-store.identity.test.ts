import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import '@/test/mocks';
import { useChatStore } from './chat-store';
import { useAuthStore } from './auth-store';
class Socket {
  static OPEN=1;
  static instances:Socket[]=[];
  readyState=1;
  onopen: null|(()=>void)=null;
  onclose:null|(()=>void)=null;
  onerror:null|(()=>void)=null;
  onmessage:null|((event:{data:string})=>void)=null;
  send=vi.fn();close=vi.fn();
  constructor(_url:string){Socket.instances.push(this);}
}
beforeEach(()=>{vi.stubGlobal('WebSocket',Socket);Socket.instances=[];useChatStore.getState().disconnect();useAuthStore.setState({jwt:'synthetic-token',user:{id:'viewer',email:'viewer@test.invalid',display_name:'Viewer',role:'employee',status:'active'},bindings:[{user_id:'viewer',agent_name:'sales',access_level:'viewer',bound_at:''}]});useChatStore.setState({selectedAgentId:'sales',messages:[],sessionId:'webchat:viewer',ownSessionId:'webchat:viewer',resumedExplicitly:false,isStreaming:false});});
afterEach(()=>{useChatStore.getState().disconnect();vi.unstubAllGlobals();});
it('does not block a directly bound Viewer from sending an own WebChat message',()=>{
  useChatStore.getState().connect();const socket=Socket.instances.at(-1)!;socket.onopen?.();
  useChatStore.getState().send('Viewer can still chat');
  expect(socket.send.mock.calls.map(([v])=>JSON.parse(v)).some(v=>v.type==='user_message'&&v.content==='Viewer can still chat'&&v.agent==='sales')).toBe(true);
});
it('drops every late frame from a disconnected account even after another socket is connected',()=>{
  useChatStore.getState().connect();const old=Socket.instances.at(-1)!;old.onopen?.();
  useChatStore.getState().disconnect();useChatStore.getState().discardHistory(true);
  useChatStore.getState().connect();const fresh=Socket.instances.at(-1)!;fresh.onopen?.();
  old.onmessage?.({data:JSON.stringify({type:'assistant_done',content:'PREVIOUS ACCOUNT PRIVATE TEXT'})});
  old.onmessage?.({data:JSON.stringify({type:'hello',session_id:'webchat:old-account'})});
  old.onclose?.();old.onerror?.();old.onopen?.();
  expect(useChatStore.getState().messages).toEqual([]);
  expect(useChatStore.getState().sessionId).not.toBe('webchat:old-account');
  expect(useChatStore.getState().connectionState).toBe('connected');
});
