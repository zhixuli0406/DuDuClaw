/**
 * Shared fixtures for the knowledge base page tests. Import `@/test/mocks`
 * first in each test file so the WebSocket client is mocked.
 */
import { render } from '@testing-library/react';
import type { ReactElement } from 'react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Route, Routes } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import type { KbCollection, KbSource } from '@/lib/knowledge-api';

export type RpcHandler = (params: Record<string, unknown>) => unknown;

/** Route `client.call(method, params)` to handlers; unknown methods reject. */
export function mockRpc(handlers: Record<string, RpcHandler>) {
  mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
    const h = handlers[method];
    if (!h) return Promise.reject(new Error(`unexpected ${method}`));
    try {
      return Promise.resolve(h(params ?? {}));
    } catch (e) {
      return Promise.reject(e);
    }
  });
}

/** Params of every call to `method`, in order. */
export function callsOf(method: string): Record<string, unknown>[] {
  return mockWsClient.call.mock.calls
    .filter((c: unknown[]) => c[0] === method)
    .map((c: unknown[]) => (c[1] ?? {}) as Record<string, unknown>);
}

export function collection(over: Partial<KbCollection> = {}): KbCollection {
  return {
    id: 'col_1',
    slug: 'hr',
    name: 'HR handbook',
    description: 'Leave and benefits',
    department: null,
    sensitivity: 'internal',
    embedding_policy: 'inherit',
    acl_revision: 3,
    created_by: 'u1',
    created_at: '2026-10-20T08:00:00.000Z',
    updated_at: '2026-10-20T08:00:00.000Z',
    my_role: 'owner',
    source_count: 2,
    ready_count: 1,
    chunk_count: 12,
    ...over,
  };
}

export function source(over: Partial<KbSource> = {}): KbSource {
  return {
    id: 'src_1',
    collection_id: 'col_1',
    kind: 'file',
    title: 'leave.pdf',
    original_name: 'leave.pdf',
    url: null,
    format: 'pdf',
    bytes: 2048,
    chars: 900,
    pages: 3,
    chunk_count: 4,
    status: 'ready',
    status_detail: 'keyword_only',
    error_code: null,
    error_message: null,
    warnings: [],
    created_by: 'u1',
    created_at: '2026-10-20T08:00:00.000Z',
    updated_at: '2026-10-20T08:00:00.000Z',
    indexed_at: '2026-10-20T08:01:00.000Z',
    job: null,
    ...over,
  };
}

/** Render inside intl + a router at `path` (pattern `route`). Each probe
 *  path renders `probe:<path>` so navigation can be asserted. */
export function renderAt(ui: ReactElement, path = '/', route = '*', probes: string[] = []) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path={route} element={ui} />
          {probes.map((p) => (
            <Route key={p} path={p} element={<div>probe:{p}</div>} />
          ))}
        </Routes>
      </MemoryRouter>
    </IntlProvider>,
  );
}
