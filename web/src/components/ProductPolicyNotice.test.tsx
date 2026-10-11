import { describe,it,expect,vi,beforeEach } from 'vitest';
import { screen,fireEvent,waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useProductPolicyStore } from '@/lib/product-policy';
import { ProductPolicyNotice } from './ProductPolicyNotice';
beforeEach(()=>{vi.clearAllMocks();useProductPolicyStore.setState({policy:null,loading:false,error:true});});
describe('product policy status',()=>{
 it('offers explicit retry after read failure and hides notice only on valid response',async()=>{
   mockWsClient.call.mockResolvedValue({profile:'futureoffice',blocked_channels:['wecom','dingtalk','feishu'],blocked_runtimes:['qwen','kimi'],blocked_providers:['minimax','deepseek']});
   renderWithProviders(<ProductPolicyNotice/>);expect(screen.getByRole('alert')).toHaveTextContent('Restricted options remain unavailable');
   fireEvent.click(screen.getByRole('button',{name:'Try again'}));await waitFor(()=>expect(screen.queryByRole('alert')).not.toBeInTheDocument());
 });
 it('rejects malformed response and retains a visible error',async()=>{
   mockWsClient.call.mockResolvedValue({profile:'futureoffice'});renderWithProviders(<ProductPolicyNotice/>);
   fireEvent.click(screen.getByRole('button',{name:'Try again'}));await waitFor(()=>expect(useProductPolicyStore.getState().error).toBe(true));
   expect(useProductPolicyStore.getState().policy).toBeNull();
 });
});
