import type { ReactElement } from 'react';
import { vi } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import type { ProtectedEmployeeList, ProtectedStepUpState } from '@/lib/api';
import { ProtectedPageProvider, type Person, type ProtectedPageContext } from './shared';
import { StepUpProvider } from './step-up';

/** Test-only helpers for the protected-mode page tests. */
export const CSRF = Array(32).fill(7);

export const PEOPLE = new Map<string, Person>([
  ['u-alice', { name: 'Alice Chen', email: 'alice@example.com' }],
  ['u-bob', { name: 'Bob Lin', email: 'bob@example.com' }],
]);

export const VERIFIED: ProtectedStepUpState = { step_up_valid: true, csrf_token: CSRF, mfa_enrolled: true };
export const NOT_VERIFIED: ProtectedStepUpState = { step_up_valid: false, csrf_token: null, mfa_enrolled: true };

export function employeeList(overrides: Partial<ProtectedEmployeeList> = {}): ProtectedEmployeeList {
  return {
    ...VERIFIED,
    employees: [
      { label: 'alice-helper', employee_id: 'a'.repeat(32), binding: { type: 'personal', owner_user_id: 'u-alice' }, disabled: false },
      { label: 'sales-team', employee_id: 'b'.repeat(32), binding: { type: 'shared' }, disabled: false },
    ],
    requests: [],
    ...overrides,
  };
}

type Handler = (params: Record<string, unknown>) => unknown;

/**
 * Route `client.call` by method. A handler that throws (or returns a rejected
 * promise) is a server refusal; an unrouted method fails loudly.
 */
export function routeCalls(handlers: Record<string, Handler>) {
  mockWsClient.call.mockImplementation(async (method: string, params?: Record<string, unknown>) => {
    const handler = handlers[method];
    if (!handler) throw new Error(`unrouted RPC in test: ${method}`);
    return handler(params ?? {});
  });
}

export function calledWith(method: string): Array<Record<string, unknown>> {
  return mockWsClient.call.mock.calls
    .filter((call: unknown[]) => call[0] === method)
    .map((call: unknown[]) => (call[1] ?? {}) as Record<string, unknown>);
}

export function renderTab(ui: ReactElement, context: Partial<ProtectedPageContext> = {}) {
  const value: ProtectedPageContext = { people: PEOPLE, goTo: vi.fn(), ...context };
  return renderWithProviders(
    <ProtectedPageProvider value={value}>
      <StepUpProvider>{ui}</StepUpProvider>
    </ProtectedPageProvider>,
  );
}
