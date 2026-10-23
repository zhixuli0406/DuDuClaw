/**
 * WebChat wire helpers (pure): which frames render into the open
 * conversation, and validation of `assistant_done`'s optional artifact.
 * Split out of `chat-store.ts`, which re-exports everything here.
 */
import type { ChatArtifact, ChatArtifactType } from '@/components/console/artifact-types';
import { browserWatchReturn } from '@/lib/browser-watch-return';

export interface ComputerWatchRef { readonly agentId: string; readonly href: string }
export function parseComputerWatch(raw: unknown): ComputerWatchRef | undefined {
  if (!raw || typeof raw !== 'object') return;
  const { agent_id: agentId, href } = raw as Record<string, unknown>;
  if (typeof agentId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(agentId)) return;
  const safe = browserWatchReturn(href);
  if (!safe || !safe.startsWith(`/computer/${agentId}?l=`)) return;
  return { agentId, href: safe };
}

/**
 * Decide whether a reply frame belongs to the conversation currently open, so a
 * long task started in conversation A doesn't render into a conversation B the
 * user switched to while it ran. Pure — exported for tests.
 *
 *  - An untagged frame (`conv` absent — legacy gateway) is always accepted.
 *  - A tagged frame is accepted only when its `conv` equals `currentConvId`;
 *    otherwise it belongs to another conversation and is dropped from the view
 *    (the gateway has already persisted it to that conversation's own history).
 */
export function frameBelongsToConversation(frameConv: unknown, currentConvId: string): boolean {
  if (typeof frameConv !== 'string' || frameConv.length === 0) return true;
  return frameConv === currentConvId;
}

/** Frame types that stream WHILE a turn runs. The first of them after a send
 *  confirms the user message is persisted (sessions-list refresh). */
export const IN_TURN_FRAME_TYPES: ReadonlySet<string> = new Set([
  'assistant_chunk',
  'step',
  'progress',
  'thinking',
  'computer_watch',
]);

/** Frame types tagged with the conversation nonce (`conv`) and dropped by the
 *  attribution guard when they belong to a conversation the user has left. */
export const CONVERSATION_FRAME_TYPES: ReadonlySet<string> = new Set([
  ...IN_TURN_FRAME_TYPES,
  'assistant_done',
]);

/** Should this frame render into the conversation currently open? Pure —
 *  exported for tests. Connection-level frames (`session_info`, `error`)
 *  always pass. */
export function shouldRenderFrame(type: unknown, frameConv: unknown, currentConvId: string): boolean {
  if (typeof type !== 'string' || !CONVERSATION_FRAME_TYPES.has(type)) return true;
  return frameBelongsToConversation(frameConv, currentConvId);
}

/** Every `ChatArtifactType` enumerated as a runtime value — `artifact-types.ts`
 *  is intentionally a pure type module with zero runtime code (see its own
 *  doc comment), so the guard below needs its own copy. Keep in sync by hand;
 *  `ChatArtifactCard`'s exhaustive switch (a `never` compile check) still
 *  catches a genuinely new artifact type being added without this list being
 *  updated — it just fails at the render call site instead of here. */
const KNOWN_ARTIFACT_TYPES: ReadonlySet<ChatArtifactType> = new Set([
  'device_status',
  'update_status',
  'backup_result',
  'network_info',
  'confirm_action',
  'update_confirm',
  'approval_request',
  'wifi_password_request',
]);

/**
 * O-4→O-3 wiring: type guard for `assistant_done`'s optional `artifact`
 * field. The gateway sends a plain JSON value (see `webchat.rs`'s
 * `ChatMessage::AssistantDone.artifact` doc comment) — this validates it into
 * a real `ChatArtifact` before it ever reaches state, so an unknown `type` or
 * a missing/malformed `payload` (an older/newer gateway build, a hand-crafted
 * frame) degrades to "no card" instead of rendering garbage or throwing.
 * Deliberately shallow: it checks `type` is one of the known artifact types
 * and `payload` is present as an object, then trusts the rest — each card
 * component in `@/components/console` already renders defensively against
 * partially-missing nested fields (the same trust boundary O-3's own fixtures
 * exercise). Pure — exported for tests.
 */
export function parseChatArtifact(raw: unknown): ChatArtifact | undefined {
  if (raw === null || typeof raw !== 'object') return undefined;
  const type = (raw as { type?: unknown }).type;
  if (typeof type !== 'string' || !KNOWN_ARTIFACT_TYPES.has(type as ChatArtifactType)) {
    return undefined;
  }
  const payload = (raw as { payload?: unknown }).payload;
  if (payload === null || typeof payload !== 'object') return undefined;
  return raw as ChatArtifact;
}
