import { client } from '@/lib/ws-client';

export const HEALTH_FAILURE_REASONS = [
  'empty_reply', 'quota', 'rate_limited', 'auth_dead', 'timeout', 'gate_refused', 'delivery', 'other',
] as const;

export interface EmployeeHealth {
  id: string;
  name: string;
  last_success_at: string | null;
  failures: Record<typeof HEALTH_FAILURE_REASONS[number], number>;
  consecutive_failures: number;
  paused_goals: number;
  account_status: 'available' | 'degraded' | 'unknown';
}

export interface EmployeeHealthSnapshot {
  agents: EmployeeHealth[];
  window_days: 1 | 7;
  partial: boolean;
}

export function loadEmployeeHealth(windowDays: 1 | 7): Promise<EmployeeHealthSnapshot> {
  return client.call('agents.health', { window_days: windowDays }) as Promise<EmployeeHealthSnapshot>;
}
