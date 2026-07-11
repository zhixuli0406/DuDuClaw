import { describe, it, expect } from 'vitest';
import { buildUserMessageFrame } from './chat-store';

describe('buildUserMessageFrame (L1 per-agent routing)', () => {
  it('omits the agent key when no partner is selected (byte-compatible)', () => {
    const frame = buildUserMessageFrame({
      content: 'hello',
      sessionId: 'webchat:x',
      agentId: null,
      attachments: [],
    });
    expect(frame).toEqual({
      type: 'user_message',
      content: 'hello',
      session_id: 'webchat:x',
      attachments: [],
    });
    expect('agent' in frame).toBe(false);
  });

  it('includes the agent id when a partner is selected', () => {
    const frame = buildUserMessageFrame({
      content: 'hi',
      sessionId: 'webchat:x',
      agentId: 'sales-bot',
      attachments: [],
    });
    expect(frame.agent).toBe('sales-bot');
    expect(frame.type).toBe('user_message');
  });

  it('maps attachments to the wire shape', () => {
    const frame = buildUserMessageFrame({
      content: '',
      sessionId: null,
      agentId: null,
      attachments: [{ name: 'a.png', mime: 'image/png', dataBase64: 'AAA' }],
    });
    expect(frame.attachments).toEqual([{ filename: 'a.png', mime: 'image/png', data_base64: 'AAA' }]);
  });
});
