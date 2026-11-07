import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import '@/test/mocks';
import { useChatStore } from './chat-store';
import { callableSubject, useCallableAgentsStore } from './callable-agents-store';
import { api } from '@/lib/api';
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
beforeEach(()=>{vi.stubGlobal('WebSocket',Socket);Socket.instances=[];useChatStore.getState().disconnect();useAuthStore.setState({jwt:'synthetic-token',user:{id:'viewer',email:'viewer@test.invalid',display_name:'Viewer',role:'employee',status:'active'},bindings:[{user_id:'viewer',agent_name:'sales',access_level:'viewer',bound_at:''}]});useChatStore.setState({selectedAgentId:'sales',resolvedAgent:{id:'sales',name:'Sales',icon:''},assignmentError:null,messages:[],sessionId:'webchat:viewer',ownSessionId:'webchat:viewer',resumedExplicitly:false,isStreaming:false});});
afterEach(()=>{useChatStore.getState().disconnect();vi.restoreAllMocks();vi.unstubAllGlobals();});
it('does not block a directly bound Viewer from sending an own WebChat message',()=>{
  useChatStore.getState().connect();const socket=Socket.instances.at(-1)!;socket.onopen?.();
  socket.onmessage?.({data:JSON.stringify({type:'session_info',session_id:'webchat:viewer',agent_name:'Sales',agent_icon:'',resolved_agent:{id:'sales',name:'Sales',icon:''},assignment_error:null})});
  useChatStore.getState().send('Viewer can still chat');
  expect(socket.send.mock.calls.map(([v])=>JSON.parse(v)).some(v=>v.type==='user_message'&&v.content==='Viewer can still chat'&&v.agent===undefined)).toBe(true);
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

it('uses server assignment metadata and keeps an explicitly resumed identity', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  useChatStore.setState({ resumedExplicitly: true, sessionId: 'webchat:old-main', selectedAgentId: 'main', resolvedAgent: { id: 'main', name: 'Main assistant', icon: '' }, agentName: 'Main assistant' });
  socket.onmessage?.({data:JSON.stringify({type:'session_info',session_id:'webchat:viewer',agent_name:'Sales',agent_icon:'',resolved_agent:{id:'sales',name:'Sales',icon:''},assignment_error:null})});
  expect(useChatStore.getState().resolvedAgent?.id).toBe('main');
  expect(useChatStore.getState().agentName).toBe('Main assistant');
  expect(useChatStore.getState().selectedAgentId).toBe('main');
  expect(useChatStore.getState().sessionId).toBe('webchat:old-main');
});
it('does not send while the server reports no assignment', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  socket.onmessage?.({data:JSON.stringify({type:'session_info',session_id:'webchat:viewer',agent_name:'',agent_icon:'',resolved_agent:null,assignment_error:{code:'no_assigned_agent',reason:'no_agents',message:'請聯絡管理員'}})});
  useChatStore.getState().send('must not send');
  expect(socket.send.mock.calls.map(([v])=>JSON.parse(v)).some(v=>v.type==='user_message')).toBe(false);
  expect(useChatStore.getState().messages).toEqual([]);
});
it('renders a structured main-session refusal as a readable message and stops sending', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  socket.onmessage?.({ data: JSON.stringify({ type: 'session_info', session_id: 'webchat:viewer', agent_name: 'Sales', agent_icon: '', resolved_agent: { id: 'sales', name: 'Sales', icon: '' }, assignment_error: null }) });
  useChatStore.getState().resumeSession('webchat:old-main', []);
  useChatStore.setState({ selectedAgentId: 'main' });
  allowChatAgent('main');
  useChatStore.getState().send('continue');
  const sent = JSON.parse(socket.send.mock.calls.at(-1)![0]);
  expect(sent.agent).toBe('main');
  socket.onmessage?.({ data: JSON.stringify({ type: 'error', code: 'main_agent_admin_only', message: 'main_agent_admin_only', conv: sent.conv, msg_id: sent.msg_id }) });
  expect(useChatStore.getState().messages.at(-1)?.content).not.toBe('main_agent_admin_only');
  expect(useChatStore.getState().assignmentError?.code).toBe('main_agent_admin_only');
  const count = socket.send.mock.calls.length;
  useChatStore.getState().send('must not retry');
  expect(socket.send).toHaveBeenCalledTimes(count);
});
it('ignores assignment metadata from a conversation that is no longer displayed', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  const previous = useChatStore.getState().convId;
  useChatStore.getState().reset();
  socket.onmessage?.({ data: JSON.stringify({ type: 'session_info', conv: previous, session_id: 'webchat:earlier', resolved_agent: { id: 'earlier', name: 'Earlier', icon: '' }, assignment_error: null }) });
  expect(useChatStore.getState().resolvedAgent).toBeUndefined();
});

it('keeps the connection base id after per-message assignment refresh so a rapid send is not a resume', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  const metadata = { type: 'session_info', agent_name: 'Sales', agent_icon: '', resolved_agent: { id: 'sales', name: 'Sales', icon: '' }, assignment_error: null };
  socket.onmessage?.({ data: JSON.stringify({ ...metadata, session_id: 'webchat:base' }) });
  useChatStore.getState().send('first');
  const first = JSON.parse(socket.send.mock.calls.at(-1)![0]);
  socket.onmessage?.({ data: JSON.stringify({ ...metadata, refresh: true, session_id: 'webchat:base#agent:sales#conv:pending', conv: first.conv, msg_id: first.msg_id }) });
  expect(useChatStore.getState().sessionId).toBe('webchat:base');
  useChatStore.getState().send('second');
  const second = JSON.parse(socket.send.mock.calls.at(-1)![0]);
  expect(second.session_id).toBe('webchat:base');
  expect(second.agent).toBeUndefined();
});
it('keeps a resumed non-main conversation usable when the default assignment requires a primary', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  useChatStore.setState({ resumedExplicitly: true, sessionId: 'webchat:old-sales', selectedAgentId: 'sales', resolvedAgent: { id: 'sales', name: 'Sales', icon: '' }, assignmentError: null });
  socket.onmessage?.({ data: JSON.stringify({ type: 'session_info', session_id: 'webchat:base', agent_name: '', agent_icon: '', resolved_agent: null, assignment_error: { code: 'no_assigned_agent', reason: 'multiple_agents', message: '' } }) });
  expect(useChatStore.getState().assignmentError).toBeNull();
  allowChatAgent('sales');
  useChatStore.getState().send('continue sales');
  const sent = JSON.parse(socket.send.mock.calls.at(-1)![0]);
  expect(sent.agent).toBe('sales');
  expect(sent.session_id).toBe('webchat:old-sales');
});

function allowChatAgent(name: string) {
  const key = JSON.stringify([callableSubject(), useChatStore.getState().sessionId]);
  useCallableAgentsStore.setState({ entries: { [key]: {
    agents: [{ name, selectable: true } as never], unrestricted: false, loading: false, confirmedAt: Date.now(),
  } } });
}

it('returns false without a frame or local message for a revoked explicit agent and refreshes chat grants', async () => {
  const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: [], unrestricted: false, default_agent: null });
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  socket.onmessage?.({ data: JSON.stringify({ type: 'session_info', session_id: 'webchat:viewer', resolved_agent: { id: 'sales', name: 'Sales', icon: '' }, assignment_error: null }) });
  useChatStore.setState({ resumedExplicitly: true, selectedAgentId: 'revoked' });
  useCallableAgentsStore.setState({ entries: { [JSON.stringify([callableSubject(), useChatStore.getState().sessionId])]: {
    agents: [], unrestricted: false, loading: false, confirmedAt: Date.now(),
  } } });
  const count = socket.send.mock.calls.length;
  expect(useChatStore.getState().send('keep my draft')).toBe(false);
  expect(socket.send).toHaveBeenCalledTimes(count);
  expect(useChatStore.getState().messages).toEqual([]);
  expect(callable).toHaveBeenCalledWith(useChatStore.getState().sessionId);
  await Promise.resolve();
  callable.mockRestore();
});

it('returns true and sends a frame without an explicit agent even without callable grants', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  socket.onmessage?.({ data: JSON.stringify({ type: 'session_info', session_id: 'webchat:viewer', resolved_agent: { id: 'sales', name: 'Sales', icon: '' }, assignment_error: null }) });
  useCallableAgentsStore.setState({ entries: {} });
  expect(useChatStore.getState().send('use assigned agent')).toBe(true);
  expect(JSON.parse(socket.send.mock.calls.at(-1)![0])).toMatchObject({ type: 'user_message', content: 'use assigned agent' });
  expect(JSON.parse(socket.send.mock.calls.at(-1)![0]).agent).toBeUndefined();
});

it('uses only fresh grants for the current chat session', () => {
  useChatStore.getState().connect();
  const socket = Socket.instances.at(-1)!;
  socket.onopen?.();
  socket.onmessage?.({ data: JSON.stringify({ type: 'session_info', session_id: 'webchat:viewer', resolved_agent: { id: 'sales', name: 'Sales', icon: '' }, assignment_error: null }) });
  useChatStore.setState({ resumedExplicitly: true });
  allowChatAgent('sales');
  expect(useChatStore.getState().send('allowed')).toBe(true);
});
