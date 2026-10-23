import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from './auth-store';
import { useChatStore } from './chat-store';
import { useConversationsStore } from './conversations-store';
const citationFetch = vi.hoisted(() => vi.fn().mockResolvedValue([]));
vi.mock('@/lib/knowledge-citations', () => ({ fetchSessionCitations: citationFetch }));
const handle = `view:v1:s:${'a'.repeat(64)}`;
const row = { session_id: handle, agent_id: 'sales', title: 'Masked conversation', last_active: '2026-10-24T00:00:00Z', turns: 2, tokens: 0, channel: 'whatsapp', read_only: true, title_masked: true };
const history = { session_id: handle, agent_id: 'sales',channel:'whatsapp',privacy_mode:'masked',read_only: true, ner: false, messages: [{ role: 'user', content: '〔PHONE 1〕', timestamp: row.last_active, tokens: 0, parts: [{ t: 'mask', cat: 'PHONE', n: 1 }] }] };
beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'me', email: 'me@test.invalid', display_name: 'Me', role: 'employee', status: 'active' }, bindings: [{ user_id: 'me', agent_name: 'sales', access_level: 'viewer', bound_at: '' }] });
  useChatStore.setState({ selectedAgentId: 'sales', sessionId: 'webchat:own', ownSessionId: 'webchat:own', messages: [], resumedExplicitly: false });
  useConversationsStore.setState({ sessions: [], status: 'idle' });
  mockWsClient.call.mockImplementation((method: string) => method === 'chat.sessions.list' ? Promise.resolve({ sessions: [row], privacy_mode: 'filtered' }) : method === 'chat.sessions.history' ? Promise.resolve(history) : Promise.resolve(null));
});
describe('bound history client privacy', () => {
  it('keeps a server-classified opaque conversation in the scoped list', async () => {
    await useConversationsStore.getState().fetch();
    expect(useConversationsStore.getState().sessions).toEqual([row]);
  });
  it('opens external history in a separate read-only view without arming chat resume or loading citations', async () => {
    expect(await useConversationsStore.getState().resume(row)).toBe(true);
    expect(useConversationsStore.getState().readOnlyView?.session_id).toBe(handle);
    expect(useChatStore.getState().sessionId).toBe('webchat:own');
    expect(useChatStore.getState().resumedExplicitly).toBe(false);
    expect(citationFetch).not.toHaveBeenCalled();
    useConversationsStore.getState().startNew();
    expect(useConversationsStore.getState().readOnlyView).toBeNull();
    expect(useChatStore.getState().resumedExplicitly).toBe(false);
  });
  it('still resumes the Viewer’s own full WebChat normally', async () => {
    const own = { ...row, session_id: 'webchat:mine', channel: 'webchat', read_only: false };
    mockWsClient.call.mockResolvedValue({ ...history, session_id: own.session_id, read_only: false, messages: [{ role: 'user', content: 'Own phone 0912345678', timestamp: row.last_active, tokens: 0 }] });
    expect(await useConversationsStore.getState().resume(own)).toBe(true);
    expect(useChatStore.getState().resumedExplicitly).toBe(true);
    expect(useChatStore.getState().messages[0].content).toContain('0912345678');
    expect(useConversationsStore.getState().readOnlyView).toBeNull();
  });
  it('clears cached list data on refusal and user changes', async () => {
    useConversationsStore.setState({ sessions: [{ ...row, session_id: 'line:old', title: 'PREVIOUS PRIVATE TITLE' }], status: 'ready' });
    mockWsClient.call.mockRejectedValue(new Error('permission denied'));
    await useConversationsStore.getState().fetch();
    expect(useConversationsStore.getState().sessions).toEqual([]);
    useChatStore.setState({ messages: [{ id: 'old', role: 'assistant', content: 'PREVIOUS PRIVATE BODY', timestamp: 0 }] });
    useAuthStore.setState({ user: { id: 'other', email: 'other@test.invalid', display_name: 'Other', role: 'employee', status: 'active' }, bindings: [] });
    expect(useConversationsStore.getState().sessions).toEqual([]);
    expect(useChatStore.getState().messages).toEqual([]);
  });
});

it('does not accept a delayed masked result after the caller starts a new conversation', async () => {
  let resolve!: (v: unknown) => void;
  mockWsClient.call.mockImplementation((method: string) => method==='chat.sessions.history' ? new Promise(r => { resolve=r; }) : Promise.resolve({sessions:[]}));
  const pending=useConversationsStore.getState().resume(row);
  useConversationsStore.getState().startNew();
  resolve(history);
  expect(await pending).toBe(false);
  expect(useConversationsStore.getState().readOnlyView).toBeNull();
});

it('resumes a server-confirmed own WebChat when reached through an opaque routing alias', async () => {
  mockWsClient.call.mockResolvedValue({ ...history,session_id:'webchat:mine',channel:'webchat',privacy_mode:'own',read_only:false,messages:[{role:'user',content:'Own full text',timestamp:row.last_active,tokens:0}] });
  expect(await useConversationsStore.getState().resume({...row,channel:'webchat',read_only:false})).toBe(true);
  expect(useChatStore.getState().sessionId).toBe('webchat:mine');
  expect(useChatStore.getState().messages[0].content).toBe('Own full text');
});

it.each([{channel:'internal'},{ner:undefined},{messages:[{role:'user',parts:[{t:'text',v:'UNTRUSTED'}],timestamp:'not a date'}]}])('refuses a malformed closed external history response: %j',async bad=>{
  mockWsClient.call.mockResolvedValue({...history,channel:'whatsapp',privacy_mode:'masked',...bad});
  expect(await useConversationsStore.getState().resume(row)).toBe(false);
  expect(useConversationsStore.getState().readOnlyView).toBeNull();
});

it('refreshes a masked view through its canonical alias without arming resume',async()=>{
  await useConversationsStore.getState().resume(row);
  await useConversationsStore.getState().refreshView();
  expect(mockWsClient.call).toHaveBeenLastCalledWith('chat.sessions.history',{session_id:handle});
  expect(useChatStore.getState().resumedExplicitly).toBe(false);
});
it.each([
  {t:'attachment',kind:'file'}, {t:'attachment',kind:'image'}, {t:'text',v:'safe text'},
])('accepts closed safe external parts without resuming: %j',async part=>{
  mockWsClient.call.mockResolvedValue({...history,messages:[{...history.messages[0],parts:[part]}]});
  expect(await useConversationsStore.getState().resume(row)).toBe(true);
});
it('drops an already-resumed full transcript when a live binding is downgraded',async()=>{
  useChatStore.setState({resumedExplicitly:true,messages:[{id:'private',role:'assistant',content:'OLD FULL TEXT',timestamp:0}]});
  useAuthStore.setState({bindings:[{user_id:'me',agent_name:'sales',access_level:'operator',bound_at:''}]});
  expect(useChatStore.getState().messages).toEqual([]);
  expect(useChatStore.getState().resumedExplicitly).toBe(false);
});

it('clears old chat state before releasing a protected history pane on an account switch',()=>{
  useChatStore.setState({resumedExplicitly:true,messages:[{id:'private',role:'assistant',content:'PREVIOUS ACCOUNT PRIVATE TEXT',timestamp:0}]});
  useConversationsStore.setState({viewOnly:true});
  let atRelease:readonly unknown[]|null=null;
  const off=useConversationsStore.subscribe((next,previous)=>{if(previous.viewOnly&&!next.viewOnly)atRelease=useChatStore.getState().messages;});
  useAuthStore.setState({user:{id:'new-user',email:'new@test.invalid',display_name:'New user',role:'employee',status:'active'},bindings:[]});
  off();expect(atRelease).toEqual([]);
});
