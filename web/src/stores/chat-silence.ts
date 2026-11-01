/** Connection heartbeats do not demonstrate progress by the active employee. */
export function isTurnActivity(frame: Record<string, unknown>, activeMessageId: string | null): boolean {
  if (frame.type === 'progress' && frame.kind === 'keepalive') return false;
  if (frame.type === 'session_info') return activeMessageId !== null && frame.msg_id === activeMessageId;
  return (activeMessageId !== null && frame.msg_id === activeMessageId)
    || ['assistant_chunk', 'assistant_done', 'step', 'progress', 'thinking', 'computer_watch', 'error'].includes(String(frame.type));
}

/** Only cross-conversation terminal failures get a content-free notification.
 * A late error for an older message in this same view stays silent. */
export function isOtherConversationFailure(
  frame: Record<string, unknown>,
  current: { convId: string; sessionId: string | null; selectedAgentId: string | null; resumedExplicitly: boolean; activeMessageId: string | null },
): boolean {
  if (frame.type !== 'error' || ['turn_not_found', 'duplicate_message', 'cancelled'].includes(String(frame.code))) return false;
  if (['turn_limit', 'identity_unavailable'].includes(String(frame.code))
    && ![frame.request_id, frame.msg_id].some(id => typeof id === 'string' && id.length > 0)) return false;
  if (current.resumedExplicitly && frame.session_id === current.sessionId) return false;
  if (typeof frame.conv === 'string' && frame.conv) return frame.conv !== current.convId;
  // With a matching message id, routing may legitimately change the session.
  if (current.activeMessageId && frame.msg_id === current.activeMessageId) return false;
  const composed = current.resumedExplicitly ? current.sessionId
    : `${current.sessionId}${current.selectedAgentId ? `#agent:${current.selectedAgentId}` : ''}#conv:${current.convId}`;
  return typeof frame.session_id === 'string' && !!frame.session_id
    && frame.session_id !== current.sessionId && frame.session_id !== composed;
}
