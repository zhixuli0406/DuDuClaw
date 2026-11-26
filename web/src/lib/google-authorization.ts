import { dashboardEndpoint } from './dashboard-base';
import { useAuthStore } from '@/stores/auth-store';

export function googleAuthorizationReturn(raw: unknown): string | undefined {
  if (typeof raw === 'string' && /^\/me\/google\/authorize#ticket=[a-f0-9]{64}$/.test(raw)) return raw;
}
export interface GoogleAuthorizationPreview {
  agent: string;
  account: string;
  purpose: 'base' | 'drive_tools' | 'chat_read' | 'workspace';
  mode: 'oauth' | 'delegate';
  services: string[];
  expires: number;
}
async function request<T>(ticket: string, confirm?: boolean): Promise<T> {
  const jwt = useAuthStore.getState().jwt;
  const response = await fetch(dashboardEndpoint('/api/me/google/authorization-request'), {
    method: confirm ? 'POST' : 'GET',
    headers: { Authorization: `Bearer ${jwt ?? ''}`, ...(confirm
      ? { 'Content-Type': 'application/json' } : { 'X-Google-Authorization-Ticket': ticket }) },
    body: confirm ? JSON.stringify({ ticket, acknowledge_google_data: true, confirm_shared_access: true }) : undefined,
    cache: 'no-store', referrerPolicy: 'no-referrer', credentials: 'omit',
  });
  if (!response.ok) throw new Error('googleAuthorization.unavailable');
  return response.json() as Promise<T>;
}
export const googleAuthorization = {
  preview: (ticket: string) => request<GoogleAuthorizationPreview>(ticket),
  confirm: (ticket: string) => request<{ completed?: boolean; authorization_url?: string }>(ticket, true),
};
