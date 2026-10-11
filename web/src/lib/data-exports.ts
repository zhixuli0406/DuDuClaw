import { client } from './ws-client';
import { useAuthStore } from '@/stores/auth-store';
export type ExportDataset = 'tasks' | 'conversations' | 'usage' | 'memories' | 'wiki';
export type ExportFormat = 'csv' | 'xlsx';
export interface ExportRequest {
  dataset: ExportDataset; format: ExportFormat; agent_id?: string;
  scope?: 'shared' | 'agent'; from?: string; until?: string;
}
export interface ExportJob {
  export_id: string; state: 'queued' | 'running' | 'ready' | 'failed' | 'cancelled' | 'expired';
  dataset: ExportDataset; format: ExportFormat; logical_rows?: number; physical_rows?: number;
  size_bytes?: number; expires_at?: string; error_code?: string;
}
export const dataExports = {
  create: (request: ExportRequest) => client.call('data_exports.create', { ...request }) as Promise<{ export_id: string }>,
  get: (export_id: string) => client.call('data_exports.get', { export_id }) as Promise<ExportJob>,
  cancel: (export_id: string) => client.call('data_exports.cancel', { export_id }) as Promise<unknown>,
};
/** Date-only controls are always Taipei calendar dates, independent of browser locale. */
export function exportDateRange(start: string, end: string): Pick<ExportRequest, 'from' | 'until'> {
  const parse = (value: string) => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) throw new Error('EXPORT_INVALID_DATE');
    const time = Date.parse(`${value}T00:00:00+08:00`);
    if (!Number.isFinite(time) || new Date(time + 8 * 3600_000).toISOString().slice(0, 10) !== value) throw new Error('EXPORT_INVALID_DATE');
    return time;
  };
  const from = start ? parse(start) : undefined;
  const until = end ? parse(end) + 86400_000 : undefined;
  if (from !== undefined && until !== undefined && until <= from) throw new Error('EXPORT_INVALID_DATE');
  return { ...(from !== undefined ? {from: new Date(from).toISOString()} : {}), ...(until !== undefined ? {until: new Date(until).toISOString()} : {}) };
}
export async function downloadDataExport(id: string, dataset: ExportDataset, format: ExportFormat): Promise<void> {
  const jwt = useAuthStore.getState().jwt;
  if (!jwt) throw new Error('EXPORT_FORBIDDEN');
  const response = await fetch(`/api/data-exports/${encodeURIComponent(id)}/download`, { headers: { Authorization: `Bearer ${jwt}` }, cache: 'no-store' });
  if (!response.ok) {
    const body: unknown = typeof response.json === 'function' ? await response.json().catch(() => null) : null;
    const code = body && typeof body === 'object' && 'error' in body ? (body as { error: unknown }).error : undefined;
    const known = new Set(['EXPORT_SOURCE_CHANGED', 'EXPORT_EXPIRED', 'EXPORT_NOT_READY', 'EXPORT_CANCELLED', 'EXPORT_PERMISSION_DENIED']);
    if (typeof code === 'string' && known.has(code)) throw new Error(code);
    throw new Error(response.status === 410 ? 'EXPORT_EXPIRED' : response.status === 401 || response.status === 403 ? 'EXPORT_FORBIDDEN' : 'EXPORT_DOWNLOAD_FAILED');
  }
  const mime = response.headers.get('content-type')?.split(';')[0].trim().toLowerCase();
  const expected = format === 'csv' ? 'text/csv' : 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
  if (mime !== expected) throw new Error('EXPORT_DOWNLOAD_FAILED');
  const blob = await response.blob();
  if (blob.size > 50 * 1024 * 1024) throw new Error('EXPORT_TOO_LARGE');
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url; anchor.download = `duduclaw-${dataset}.${format}`;
  document.body.append(anchor); anchor.click(); anchor.remove();
  // Give the browser time to hand the object URL to its download subsystem.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
