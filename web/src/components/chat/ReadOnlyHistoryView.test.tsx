import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConversationsStore } from '@/stores/conversations-store';
import { ReadOnlyHistoryView } from './ReadOnlyHistoryView';
const data = { session_id:`view:v1:s:${'a'.repeat(64)}`,agent_id:'sales',channel:'whatsapp',read_only:true,ner:false,messages:[{role:'user',timestamp:'2026-10-24T01:00:00Z',tokens:0,content:'UNSAFE FALLBACK MUST NOT SHOW',sender_label:'Customer 1',parts:[{t:'text',v:'<img src=x onerror=alert(1)>'},{t:'mask',cat:'PHONE',n:1},{t:'attachment',kind:'file',name:'PRIVATE-FILENAME.pdf'}]}] };
beforeEach(() => {useConversationsStore.setState({viewOnly:true,viewLoading:false,viewError:false,readOnlyView:data as never});});
describe('external history view', () => {
  it('shows only closed plain parts with a masking limit notice and no composer, links or file names', () => {
    const {container}=renderWithProviders(<ReadOnlyHistoryView/>);
    expect(screen.getByText('External conversation (read-only)')).toBeInTheDocument();
    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeInTheDocument();
    expect(screen.getByText('Phone 1')).toBeInTheDocument();
    expect(screen.getByText(/AI detection is not installed/)).toBeInTheDocument();
    expect(container.querySelector('img, a, textarea, audio, video, iframe')).toBeNull();
    expect(screen.queryByText(/PRIVATE-FILENAME|UNSAFE FALLBACK/)).not.toBeInTheDocument();
  });
  it('clears content on loading or failure and can return to a new interactive conversation', () => {
    const start=vi.spyOn(useConversationsStore.getState(),'startNew').mockImplementation(()=>{});
    useConversationsStore.setState({viewLoading:true,readOnlyView:null});
    const {rerender}=renderWithProviders(<ReadOnlyHistoryView/>);
    expect(screen.queryByText('Phone 1')).not.toBeInTheDocument();
    useConversationsStore.setState({viewLoading:false,viewError:true,readOnlyView:null});
    rerender(<ReadOnlyHistoryView/>);
    expect(screen.getByRole('alert')).toHaveTextContent('History is unavailable');
    fireEvent.click(screen.getByRole('button',{name:'New conversation'}));
    expect(start).toHaveBeenCalledOnce();
  });
});

it('refreshes metadata safely and uses current staff names without a false regex-only warning', () => {
  const refresh=vi.spyOn(useConversationsStore.getState(),'refreshView').mockResolvedValue();
  useConversationsStore.setState({readOnlyView:{...data,ner:true,messages:[{...data.messages[0],role:'assistant',sender_label:'',timestamp:'invalid',parts:[{t:'attachment',kind:'image'}]}]}} as never);
  renderWithProviders(<ReadOnlyHistoryView/>);
  expect(screen.queryByText(/AI detection is not installed/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button',{name:'Refresh'}));
  expect(refresh).toHaveBeenCalledOnce();
});

it('formats assistant text parts while retaining masks and hiding raw content and attachment names', () => {
  useConversationsStore.setState({readOnlyView:{...data,messages:[{...data.messages[0],role:'assistant',parts:[{t:'text',v:'**Formatted**\n\n| a |\n| --- |\n| b |'},{t:'mask',cat:'PHONE',n:1},{t:'attachment',kind:'file',name:'PRIVATE-FILENAME.pdf'}]}]}} as never);
  const {container}=renderWithProviders(<ReadOnlyHistoryView/>);
  expect(container.querySelector('strong')).toHaveTextContent('Formatted');
  expect(screen.getByRole('table')).toBeInTheDocument();
  expect(screen.getByText('Phone 1')).toBeInTheDocument();
  expect(screen.queryByText(/PRIVATE-FILENAME|UNSAFE FALLBACK/)).not.toBeInTheDocument();
});

it('renders failure notices as plain system status instead of staff replies', () => {
  useConversationsStore.setState({readOnlyView:{...data,messages:[{...data.messages[0],role:'assistant',notice_kind:'turn_failure',parts:[{t:'text',v:'**This turn received no reply.**'}]}]}} as never);
  const {container}=renderWithProviders(<ReadOnlyHistoryView/>);
  expect(screen.getByText('System notice').closest('li')).toHaveTextContent('**This turn received no reply.**');
  expect(container.querySelector('strong')).toBeNull();
  expect(screen.getByText('System notice').closest('li')).toHaveClass('text-destructive');
});
