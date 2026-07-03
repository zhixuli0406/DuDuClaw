import { describe, it, expect, beforeEach } from 'vitest';
import { useCommandPaletteStore } from './command-palette-store';

describe('command-palette-store', () => {
  beforeEach(() => {
    localStorage.clear();
    useCommandPaletteStore.setState({ open: false, recent: [] });
  });

  it('opens, closes and toggles', () => {
    const s = useCommandPaletteStore.getState();
    s.openPalette();
    expect(useCommandPaletteStore.getState().open).toBe(true);
    s.closePalette();
    expect(useCommandPaletteStore.getState().open).toBe(false);
    s.toggle();
    expect(useCommandPaletteStore.getState().open).toBe(true);
  });

  it('records visits most-recent-first', () => {
    const { recordVisit } = useCommandPaletteStore.getState();
    recordVisit('/agents');
    recordVisit('/tasks');
    expect(useCommandPaletteStore.getState().recent).toEqual(['/tasks', '/agents']);
  });

  it('de-dupes a re-visited route to the front', () => {
    const { recordVisit } = useCommandPaletteStore.getState();
    recordVisit('/agents');
    recordVisit('/tasks');
    recordVisit('/agents');
    expect(useCommandPaletteStore.getState().recent).toEqual(['/agents', '/tasks']);
  });

  it('caps the recent list at 6 entries', () => {
    const { recordVisit } = useCommandPaletteStore.getState();
    ['/a', '/b', '/c', '/d', '/e', '/f', '/g'].forEach(recordVisit);
    const recent = useCommandPaletteStore.getState().recent;
    expect(recent).toHaveLength(6);
    expect(recent[0]).toBe('/g');
    expect(recent).not.toContain('/a');
  });

  it('persists the recent list to localStorage', () => {
    useCommandPaletteStore.getState().recordVisit('/memory');
    expect(JSON.parse(localStorage.getItem('duduclaw-cmdk-recent') ?? '[]')).toEqual(['/memory']);
  });
});
