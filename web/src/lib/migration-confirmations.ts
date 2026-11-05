import { client } from './ws-client';

export interface ProposalBinding {
  operation_id: { proposal_id: number[]; revision: number };
  proposal_digest: number[];
}
export type ProposalStatus = 'pending' | 'approved' | 'applying' | 'applied' | 'failed' | 'rejecting' | 'rejected' | 'expired_rejected';
export interface ConfirmationItem {
  binding: ProposalBinding;
  category: string;
  status: ProposalStatus;
  high_risk: boolean;
  apply_available: boolean;
  rotation_pending: boolean;
  content_status?: 'not_migrated_rejected' | 'awaiting_materialization' | 'migrated';
  fields: Array<{ name: string; value: { kind: 'text'; value: string } | { kind: 'secret_fingerprint'; keyed_fingerprint: number[]; needs_rotation: boolean } }>;
}
export interface ConfirmationList {
  active: boolean;
  incomplete: boolean;
  items: ConfirmationItem[];
  mutations_available: boolean;
  csrf_token: number[] | null;
  mfa_enrolled: boolean;
  step_up_valid: boolean;
}
export const proposalKey = (item: ConfirmationItem) => `${item.binding.operation_id.proposal_id.join('-')}:${item.binding.operation_id.revision}`;
export const bulkEligible = (item: ConfirmationItem) => item.status === 'pending' && !item.high_risk && item.apply_available;
export const confirmationApi = {
  list: () => client.call('migration.confirmations.list', {}) as Promise<ConfirmationList>,
  stepUp: (password: string, code: string) => client.call('migration.confirmations.step_up', { password, code }),
  rotateSecret: (binding: ProposalBinding, new_value: string, csrf_token: number[]) =>
    client.call('migration.confirmations.rotate_secret', { binding, new_value, csrf_token }),
  decide: (action: 'approve' | 'retry' | 'reject', items: ConfirmationItem[], csrf_token: number[]) =>
    client.call(`migration.confirmations.${action}`, { bindings: items.map(item => item.binding), csrf_token }),
};
