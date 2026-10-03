import { describe, it, expect } from 'vitest';
import { draftFromConfig, draftToUpsert, emptyDraft, parseHeaders } from './tickSourceForm';

describe('draftToUpsert', () => {
  it('builds an http_poll payload and keeps stored headers when none are typed', () => {
    const d = {
      ...emptyDraft(),
      id: 'btc-price',
      url: 'https://example.com/api',
      intervalSecs: '30',
      jsonFields: 'price = /data/price\nvol = /data/volume',
      maxEventsPerMinute: '',
    };
    const r = draftToUpsert(d);
    expect('payload' in r).toBe(true);
    if (!('payload' in r)) return;
    expect(r.payload).toEqual({
      id: 'btc-price',
      kind: 'http_poll',
      enabled: true,
      url: 'https://example.com/api',
      command: null,
      path: null,
      interval_secs: 30,
      max_events_per_minute: null,
      baseline_max_age_secs: null,
      json_fields: { price: '/data/price', vol: '/data/volume' },
      subscribe: null,
    });
    expect('headers' in r.payload).toBe(false);
  });

  it('replaces headers when typed and removes them when cleared', () => {
    const base = { ...emptyDraft(), id: 'feed', kind: 'websocket' as const, url: 'wss://x.test/ws', intervalSecs: '' };
    const typed = draftToUpsert({ ...base, headersText: 'Authorization: secret://env/TOKEN' });
    expect('payload' in typed && typed.payload.headers).toEqual({ Authorization: 'secret://env/TOKEN' });
    const cleared = draftToUpsert({ ...base, clearHeaders: true, headersCount: 2 });
    expect('payload' in cleared && cleared.payload.headers).toBeNull();
  });

  it('sends a command source as argv, one argument per line', () => {
    const r = draftToUpsert({ ...emptyDraft(), id: 'disk', kind: 'command', command: 'df\n -h \n\n' });
    expect('payload' in r && r.payload.command).toEqual(['df', '-h']);
    expect('payload' in r && r.payload.url).toBeNull();
  });

  it('names the first problem without echoing header values', () => {
    expect(draftToUpsert({ ...emptyDraft(), id: 'Bad_ID', url: 'https://x' })).toEqual({ error: 'id' });
    expect(draftToUpsert({ ...emptyDraft(), id: 'ok' })).toEqual({ error: 'url' });
    expect(draftToUpsert({ ...emptyDraft(), id: 'ok', url: 'https://x', intervalSecs: '0' })).toEqual({ error: 'interval' });
    const bad = draftToUpsert({ ...emptyDraft(), id: 'ok', url: 'https://x', headersText: 'X-Ok: 1\nsupersecretvalue' });
    expect(bad).toEqual({ error: 'headers', detail: '2' });
    expect(parseHeaders('no colon here')).toEqual({ badLine: 1 });
  });
});

describe('draftFromConfig', () => {
  it('round-trips a listed source into editable text', () => {
    const d = draftFromConfig({
      id: 'disk',
      kind: 'command',
      command: ['df', '-h'],
      json_fields: { used: '/used' },
      headers_count: 0,
      enabled: false,
    });
    expect(d.command).toBe('df\n-h');
    expect(d.jsonFields).toBe('used = /used');
    expect(d.enabled).toBe(false);
  });
});
