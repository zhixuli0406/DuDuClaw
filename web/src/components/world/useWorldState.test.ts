import { describe, it, expect } from 'vitest';
import { mapAgentToWorldState, emoteForLive, type WorldInputAgent } from './useWorldState';
import { seatTileFor, coffeeTile, DESK_CAP, getScene } from './rooms';
import { characterFor } from '@/lib/character-gen';

const office = getScene('office');

const agent = (over: Partial<WorldInputAgent> = {}): WorldInputAgent => ({
  name: 'scout',
  display_name: 'Scout',
  status: 'active',
  ...over,
});

describe('emoteForLive', () => {
  it('working states show the working emote', () => {
    expect(emoteForLive('active', 'tool_running')).toBe('working');
    expect(emoteForLive('active', 'replying')).toBe('working');
    expect(emoteForLive('active', 'consolidating')).toBe('working');
  });
  it('awaiting approval shows the hand emote', () => {
    expect(emoteForLive('active', 'awaiting_approval')).toBe('awaiting');
  });
  it('idle active shows nothing', () => {
    expect(emoteForLive('active', 'idle')).toBeNull();
  });
  it('paused shows sleeping regardless of live signal', () => {
    expect(emoteForLive('paused', 'tool_running')).toBe('sleeping');
  });
  it('terminated shows nothing', () => {
    expect(emoteForLive('terminated', 'tool_running')).toBeNull();
  });
});

describe('mapAgentToWorldState (office behaviour map §8.2)', () => {
  it('active + live run → seated & working at its desk', () => {
    const s = mapAgentToWorldState(office, agent(), 0, 'tool_running')!;
    const seat = seatTileFor(0);
    expect(s).toMatchObject({ x: seat.x, y: seat.y, action: 'sitting', emote: 'working', dimmed: false });
  });

  it('active + idle → seated, calm (no emote)', () => {
    const s = mapAgentToWorldState(office, agent(), 0, 'idle')!;
    expect(s.action).toBe('sitting');
    expect(s.emote).toBeNull();
  });

  it('paused → loiters by the coffee machine, sleeping', () => {
    const s = mapAgentToWorldState(office, agent({ status: 'paused' }), 0, 'idle')!;
    const coffee = coffeeTile();
    expect(s.action).toBe('idle');
    expect(s.emote).toBe('sleeping');
    // Placed near (not necessarily on) the coffee tile.
    expect(Math.abs(s.x - coffee.x)).toBeLessThanOrEqual(2);
    expect(Math.abs(s.y - coffee.y)).toBeLessThanOrEqual(1);
  });

  it('terminated → seated but dimmed (empty-desk read)', () => {
    const s = mapAgentToWorldState(office, agent({ status: 'terminated' }), 0, 'idle')!;
    expect(s.dimmed).toBe(true);
    expect(s.emote).toBeNull();
  });

  it('overflow agent (index ≥ desk cap) is placed near coffee even when active', () => {
    const s = mapAgentToWorldState(office, agent(), DESK_CAP, 'tool_running')!;
    expect(s.action).toBe('idle');
  });

  it('tint index matches the shared character-gen source (world ⇄ UI parity)', () => {
    const s = mapAgentToWorldState(office, agent(), 0, 'idle')!;
    expect(s.tintIndex).toBe(characterFor('scout').tintIndex);
  });

  it('carries the say bubble text through untouched', () => {
    const s = mapAgentToWorldState(office, agent(), 0, 'replying', 'hi')!;
    expect(s.say).toBe('hi');
  });
});
