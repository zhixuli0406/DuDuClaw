import { client } from './ws-client';
import { useAuthStore } from '@/stores/auth-store';

/**
 * Task RPCs that may carry files (`tasks.goal_create`, `tasks.skill_assign`).
 *
 * The dashboard RPC socket refuses frames over 1 MiB, far below the 20 MB a
 * single attachment may be. A request WITH files therefore goes to
 * `POST /api/tasks/rpc`, which the gateway dispatches through the same
 * handler as the socket; a request without files stays on the socket.
 */

/** One file as the gateway expects it (base64 without the `data:` prefix). */
export interface TaskAttachmentInput {
  name: string;
  mime: string;
  data_base64: string;
}

export const TASK_RPC_PATH = '/api/tasks/rpc';

export async function callTaskRpc<T>(
  method: 'tasks.goal_create' | 'tasks.skill_assign',
  params: { attachments?: readonly TaskAttachmentInput[] } & Record<string, unknown>,
): Promise<T> {
  const { attachments, ...rest } = params;
  if (!attachments || attachments.length === 0) {
    return client.call(method, rest) as Promise<T>;
  }
  const jwt = useAuthStore.getState().jwt;
  const res = await fetch(TASK_RPC_PATH, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}),
    },
    body: JSON.stringify({ method, params: { ...rest, attachments } }),
  });
  type Frame = { ok?: boolean; payload?: unknown; error?: unknown };
  const body: Frame | null = await res.json().then(
    (v: unknown) => (v && typeof v === 'object' ? (v as Frame) : null),
    () => null,
  );
  if (res.ok && body?.ok === true) return body.payload as T;
  const message =
    typeof body?.error === 'string' && body.error
      ? body.error
      : res.status === 413
        ? 'attachments_too_large'
        : `HTTP ${res.status}`;
  throw new Error(message);
}
