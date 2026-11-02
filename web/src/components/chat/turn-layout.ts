import type { ChatMessage } from '@/stores/chat-store';

/**
 * Split the transcript around the current turn (pure — exported for tests):
 *  - `streamingAssistant`: a reply still being written (rendered below the
 *    live task insights);
 *  - `turnReply`: the finished reply of the turn just completed, when it
 *    carries thinking — the task-insights box (with its "thought for N s"
 *    line) sits above it, so the line reads before the answer;
 *  - `prior`: everything else, in order. Earlier replies show their own
 *    compact thinking line.
 */
export function splitTurnMessages(
  messages: readonly ChatMessage[],
  isStreaming: boolean,
  activeMessageId?: string | null,
): {
  prior: readonly ChatMessage[];
  following: readonly ChatMessage[];
  streamingAssistant: ChatMessage | null;
  turnReply: ChatMessage | null;
} {
  const userIndex = activeMessageId ? messages.findIndex(m => m.role === 'user' && m.turnId === activeMessageId) : -1;
  const end = userIndex >= 0
    ? (messages[userIndex + 1]?.role === 'assistant' && messages[userIndex + 1]?.turnId === activeMessageId ? userIndex + 2 : userIndex + 1)
    : messages.findIndex(m => m.role === 'user' && m.delivery === 'queued');
  const visible = end >= 0 ? messages.slice(0, end) : messages;
  const following = end >= 0 ? messages.slice(end) : [];
  const last = visible[visible.length - 1];
  const isAssistant = last?.role === 'assistant';
  const streamingAssistant = isStreaming && isAssistant && last.tokens == null && (!last.turnId || last.turnId === activeMessageId) ? last : null;
  const turnReply = !isStreaming && isAssistant && last.thinking ? last : null;
  const prior = streamingAssistant || turnReply ? visible.slice(0, -1) : visible;
  return { prior, following, streamingAssistant, turnReply };
}
