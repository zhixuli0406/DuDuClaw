import { beforeEach,describe,it,expect,vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useProductPolicyStore } from '@/lib/product-policy';
import { AddChannelDialog } from '@/components/channels/AddChannelDialog';
import { AccountsPage } from '@/pages/AccountsPage';
beforeEach(()=>{vi.clearAllMocks();mockWsClient.call.mockResolvedValue({});useProductPolicyStore.setState({policy:{profile:'futureoffice',blocked_channels:['wecom','dingtalk','feishu'],blocked_runtimes:['qwen','kimi'],blocked_providers:['minimax','deepseek']},loading:false,error:false});});
describe('FutureOffice visible pickers',()=>{
 it('hides the three named channels while offering LINE',async()=>{
   const user=userEvent.setup();renderWithProviders(<AddChannelDialog open onClose={()=>{}} onCreated={()=>{}}/>);
   await user.click(screen.getAllByRole('combobox')[0]);
   expect(await screen.findByRole('option',{name:'LINE'})).toBeInTheDocument();
   expect(screen.queryByRole('option',{name:'Feishu'})).not.toBeInTheDocument();
   expect(screen.queryByRole('option',{name:/WeCom/})).not.toBeInTheDocument();
   expect(screen.queryByRole('option',{name:/DingTalk/})).not.toBeInTheDocument();
 });
 it('hides MiniMax and DeepSeek API choices while retaining Qwen API',async()=>{
   const user=userEvent.setup();renderWithProviders(<AccountsPage/>);
   await user.click(screen.getByRole('button',{name:/Add account/i}));
   await user.click(screen.getAllByRole('combobox')[0]);
   expect(await screen.findByRole('option',{name:'Qwen (DashScope)'})).toBeInTheDocument();
   expect(screen.queryByRole('option',{name:'DeepSeek'})).not.toBeInTheDocument();
   expect(screen.queryByRole('option',{name:'MiniMax'})).not.toBeInTheDocument();
 });
});
