import { beforeEach,describe,it,expect,vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useProductPolicyStore } from '@/lib/product-policy';
import { AddChannelDialog } from '@/components/channels/AddChannelDialog';
import { AccountsPage } from '@/pages/AccountsPage';
beforeEach(()=>{vi.clearAllMocks();mockWsClient.call.mockResolvedValue({});useProductPolicyStore.setState({policy:{profile:'restricted',blocked_channels:['slack'],blocked_runtimes:['claude'],blocked_providers:['openai']},loading:false,error:false});});
describe('gateway policy visible pickers',()=>{
 it('hides the configured channel while offering LINE',async()=>{
   const user=userEvent.setup();renderWithProviders(<AddChannelDialog open onClose={()=>{}} onCreated={()=>{}}/>);
   await user.click(screen.getAllByRole('combobox')[0]);
   expect(await screen.findByRole('option',{name:'LINE'})).toBeInTheDocument();
   expect(screen.queryByRole('option',{name:'Slack'})).not.toBeInTheDocument();
 });
 it('hides the configured API provider while retaining the other catalog choices',async()=>{
   const user=userEvent.setup();renderWithProviders(<AccountsPage/>);
   await user.click(screen.getByRole('button',{name:/Add account/i}));
   await user.click(screen.getAllByRole('combobox')[0]);
   expect(await screen.findByRole('option',{name:'Claude (Anthropic)'})).toBeInTheDocument();
   expect(screen.queryByRole('option',{name:'OpenAI'})).not.toBeInTheDocument();
   expect(screen.getByRole('option',{name:'DeepSeek'})).toBeInTheDocument();
   expect(screen.getByRole('option',{name:'MiniMax'})).toBeInTheDocument();
 });
});
