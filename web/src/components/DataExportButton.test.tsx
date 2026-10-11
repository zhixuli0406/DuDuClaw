import { describe,it,expect,vi,beforeEach } from 'vitest';
import { screen,fireEvent,waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { DataExportButton } from './DataExportButton';
const download=vi.fn();
vi.mock('@/lib/data-exports',async original=>({...await original<typeof import('@/lib/data-exports')>(),downloadDataExport:(...args:unknown[])=>download(...args)}));
beforeEach(()=>{vi.clearAllMocks();mockWsClient.call.mockImplementation(async(method:string)=>method==='agents.list'?{agents:[]} : method==='data_exports.create'?{export_id:'ex-1'} : {export_id:'ex-1',state:'ready',dataset:'usage',format:'xlsx',logical_rows:3,physical_rows:4,expires_at:'2026-10-11T01:00:00Z'});});
describe('export journey',()=>{
 it('creates a native Excel export with Taipei date filter, then downloads ready file',async()=>{
  renderWithProviders(<DataExportButton dataset="usage"/>);fireEvent.click(screen.getByRole('button',{name:'Export'}));
  fireEvent.change(screen.getByLabelText('From date'),{target:{value:'2026-10-11'}});
  fireEvent.change(screen.getByLabelText('Through date (inclusive)'),{target:{value:'2026-10-11'}});
  fireEvent.click(screen.getByRole('button',{name:'Create export'}));
  await screen.findByText('Ready to download');
  expect(mockWsClient.call).toHaveBeenCalledWith('data_exports.create',{dataset:'usage',format:'xlsx',from:'2026-10-10T16:00:00.000Z',until:'2026-10-11T16:00:00.000Z'});
  expect(screen.getByText('3 records; 4 output rows.')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button',{name:'Download'}));await waitFor(()=>expect(download).toHaveBeenCalledWith('ex-1','usage','xlsx'));
 });
 it('cancels queued work and never offers incomplete download',async()=>{
  mockWsClient.call.mockImplementation(async(method:string)=>method==='agents.list'?{agents:[]} : method==='data_exports.create'?{export_id:'ex-1'} : method==='data_exports.cancel'?{} : {export_id:'ex-1',state:mockWsClient.call.mock.calls.some(c=>c[0]==='data_exports.cancel')?'cancelled':'queued',dataset:'tasks',format:'csv'});
  renderWithProviders(<DataExportButton dataset="tasks"/>);fireEvent.click(screen.getByRole('button',{name:'Export'}));fireEvent.click(screen.getByRole('button',{name:'Create export'}));await screen.findByText('Queued');
  expect(screen.queryByRole('button',{name:'Download'})).not.toBeInTheDocument();fireEvent.click(screen.getByRole('button',{name:'Cancel export'}));await screen.findByText('Export cancelled.');
 });
 it('reports permission failure without exposing internal details',async()=>{
  mockWsClient.call.mockImplementation(async(method:string)=>{if(method==='agents.list')return{agents:[]};throw new Error('EXPORT_FORBIDDEN');});
  renderWithProviders(<DataExportButton dataset="wiki" scope="shared"/>);fireEvent.click(screen.getByRole('button',{name:'Export'}));fireEvent.click(screen.getByRole('button',{name:'Create export'}));
  expect(await screen.findByRole('alert')).toHaveTextContent('access denied');
 });
});
