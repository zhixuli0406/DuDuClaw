import { useAuthStore } from '@/stores/auth-store';
import { chunkSha256 } from './sha256';

export interface MeetingJob {
  job_id: string;
  state: 'queued' | 'preparing' | 'transcribing' | 'done' | 'failed' | 'cancelled';
  chunk: number;
  chunks: number;
  created_at: number;
  error?: string | null;
  result?: { duration_ms?: number; speaker_count?: number } | null;
}
export interface UploadReceipt { upload_id: string; chunk_size: number; received: number[]; hashes?: Record<string, string>; expires_at?: number }
export class MeetingHttpError extends Error { constructor(message: string, readonly status: number) { super(message); } }
export async function meetingFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const jwt = useAuthStore.getState().jwt;
  const headers = new Headers(init.headers);
  if (jwt) headers.set('Authorization', `Bearer ${jwt}`);
  const response = await fetch(path, { ...init, headers });
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { message?: string; error?: string };
    throw new MeetingHttpError(body.message || body.error || 'meeting.requestFailed', response.status);
  }
  return response;
}
export const meetingApi = {
  list: async (agent: string): Promise<{ jobs: MeetingJob[]; max_bytes: number }> =>
    (await meetingFetch(`/api/meeting-jobs?agent=${encodeURIComponent(agent)}`)).json(),
  cancel: async (agent: string, id: string): Promise<MeetingJob> =>
    (await meetingFetch(`/api/meeting-jobs/${encodeURIComponent(id)}/cancel?agent=${encodeURIComponent(agent)}`, { method: 'POST' })).json(),
  download: async (agent: string, id: string, format: 'md' | 'json') => {
    const response = await meetingFetch(`/api/meeting-jobs/${encodeURIComponent(id)}/download?agent=${encodeURIComponent(agent)}&format=${format}`);
    const url = URL.createObjectURL(await response.blob());
    const anchor = document.createElement('a');
    anchor.href = url; anchor.download = `transcript-${id}.${format}`;
    document.body.appendChild(anchor); anchor.click(); anchor.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 10_000);
  },
};

// The receipt is scoped to the signed-in person, employee, and exact file metadata.
// It contains no credential. Server-side ownership remains authoritative.
export function uploadStorageKey(user: string, agent: string, file: File): string {
  return `meeting-upload:${JSON.stringify([user, agent, file.name, file.size, file.lastModified])}`;
}
export async function uploadMeeting(file: File, agent: string, user: string, onProgress: (fraction: number) => void, signal?: AbortSignal): Promise<MeetingJob> {
  const key = uploadStorageKey(user, agent, file);
  let saved: string | null = null;
  try { saved = localStorage.getItem(key); } catch { /* Storage is optional. */ }
  let receipt: UploadReceipt | undefined;
  if (saved) {
    try { receipt = await (await meetingFetch(`/api/meeting-uploads/${encodeURIComponent(saved)}`, { signal })).json(); }
    catch (error) {
      if (!(error instanceof MeetingHttpError) || ![404, 410].includes(error.status)) throw error;
      try { localStorage.removeItem(key); } catch { /* Storage is optional. */ }
    }
  }
  if (!receipt) {
    receipt = await (await meetingFetch('/api/meeting-uploads', { method: 'POST', signal,
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ agent_id: agent, file_name: file.name, size: file.size }) })).json() as UploadReceipt;
    try { localStorage.setItem(key, receipt.upload_id); } catch { /* Storage is optional. */ }
  }
  if (!Number.isSafeInteger(receipt.chunk_size) || receipt.chunk_size < 1 || receipt.chunk_size > 8 * 1024 * 1024) throw new Error('meeting.requestFailed');
  const upload = receipt;
  const count = Math.ceil(file.size / upload.chunk_size);
  const received = new Set(receipt.received.filter(i => Number.isSafeInteger(i) && i >= 0 && i < count));
  let bytes = [...received].reduce((sum, i) => sum + Math.min(upload.chunk_size, file.size - i * upload.chunk_size), 0);
  onProgress(bytes / file.size);
  for (let index = 0; index < count; index++) {
    signal?.throwIfAborted();
    const chunk = await file.slice(index * receipt.chunk_size, (index + 1) * receipt.chunk_size).arrayBuffer();
    const hash = await chunkSha256(chunk);
    if (received.has(index)) {
      if (upload.hashes?.[String(index)] !== hash) throw new Error('meeting.fileChanged');
      continue;
    }
    await meetingFetch(`/api/meeting-uploads/${encodeURIComponent(receipt.upload_id)}/${index}`, {
      method: 'PUT', body: chunk, signal, headers: { 'Content-Type': 'application/octet-stream', 'X-Chunk-SHA256': hash },
    });
    bytes += chunk.byteLength; onProgress(bytes / file.size);
  }
  const job = await (await meetingFetch(`/api/meeting-uploads/${encodeURIComponent(receipt.upload_id)}/complete`, { method: 'POST', signal })).json() as MeetingJob;
  try { localStorage.removeItem(key); } catch { /* Storage is optional. */ }
  return job;
}
