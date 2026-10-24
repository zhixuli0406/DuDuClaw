import { describe, it, expect } from 'vitest';
import { sessionChannel, isConversationSession } from './session-channel';

describe('session-channel', () => {
  it('maps known channel prefixes to labels', () => {
    expect(sessionChannel('telegram:12345')?.label).toBe('Telegram');
    expect(sessionChannel('discord:thread:99')?.label).toBe('Discord');
    expect(sessionChannel('webchat:conn#agent:x#conv:1')?.label).toBe('Web');
    expect(sessionChannel('line:group:abc')?.label).toBe('LINE');
  });

  it('rejects internal work sessions and malformed ids', () => {
    expect(sessionChannel('cu-0af31')).toBeNull();
    expect(sessionChannel('some-agent-run')).toBeNull();
    expect(sessionChannel(':leading-colon')).toBeNull();
    expect(sessionChannel('')).toBeNull();
    expect(isConversationSession('telegram:1')).toBe(true);
    expect(isConversationSession('goal:1')).toBe(false);
  });
});

it('prefers explicit server channels for opaque routes and refuses an unknown explicit class', () => {
  expect(sessionChannel('view:v1:s:abcdef','whatsapp')?.label).toBe('WhatsApp');
  expect(sessionChannel('view:v1:s:abcdef','widget')?.label).toBe('Website visitor');
  expect(sessionChannel('webchat:misleading','internal')).toBeNull();
});
