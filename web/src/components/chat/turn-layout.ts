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
): {
  prior: readonly ChatMessage[];
  streamingAssistant: ChatMessage | null;
  turnReply: ChatMessage | null;
} {
  const last = messages[messages.length - 1];
  const isAssistant = last?.role === 'assistant';
  const streamingAssistant = isStreaming && isAssistant && last.tokens == null ? last : null;
  const turnReply = !isStreaming && isAssistant && last.thinking ? last : null;
  const prior = streamingAssistant || turnReply ? messages.slice(0, -1) : messages;
  return { prior, streamingAssistant, turnReply };
}
