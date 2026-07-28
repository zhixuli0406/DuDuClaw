/**
 * PetRuntime autonomous-wander engine — deterministic fake-timer tests.
 *
 * The engine must leave `idle` on its own (walk / rest / wave / jump) and
 * come back. A field report ("桌寵不會走動、坐下") motivated pinning this
 * with real clock advancement instead of code reading.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, act, cleanup } from '@testing-library/react';
import { PetRuntime } from './PetRuntime';
import type { PetRuntimePayload } from '@/lib/pet';

const petMoveBy = vi.fn(async (_dx: number) => false);
const isTauriMock = vi.fn(() => true);

vi.mock('@/lib/pet', () => ({
  isTauri: () => isTauriMock(),
  petMoveBy: (dx: number) => petMoveBy(dx),
  onPetAgentSignal: async () => () => {},
  openPetContextMenu: async () => {},
  startWindowDrag: async () => {},
}));

const proceduralPet: PetRuntimePayload = {
  slug: 'test-pet',
  displayName: '測試寵',
  mode: 'procedural',
  imageDataUrl: 'data:image/png;base64,iVBORw0KGgo=',
  spriteSheet: null,
} as unknown as PetRuntimePayload;

function currentState(container: HTMLElement): string {
  return container.querySelector('[data-pet-state]')!.getAttribute('data-pet-state')!;
}

describe('PetRuntime wander engine', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    petMoveBy.mockClear();
    isTauriMock.mockReturnValue(true);
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('leaves idle on its own within the first pick window', async () => {
    // Force the non-idle branch deterministically: r=0.5 → rest.
    vi.spyOn(Math, 'random').mockReturnValue(0.5);
    const { container } = render(<PetRuntime pet={proceduralPet} />);
    expect(currentState(container)).toBe('idle');
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000); // > WANDER_MIN+EXTRA at r=0.5
    });
    expect(currentState(container)).toBe('rest');
    // …and settles back to idle after the rest window.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(currentState(container)).toBe('idle');
  });

  it('walks (moving the window) and stops at the walk deadline', async () => {
    vi.spyOn(Math, 'random').mockReturnValue(0.1); // walk branch, dir = -1
    const { container } = render(<PetRuntime pet={proceduralPet} />);
    // Pick fires at 6000 + 0.1·8000 = 6.8s; walk deadline 2.4s later (9.2s).
    await act(async () => {
      await vi.advanceTimersByTimeAsync(7_000);
    });
    expect(currentState(container)).toBe('walk-left');
    expect(petMoveBy).toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3_000); // past the walk deadline
    });
    expect(currentState(container)).toBe('idle');
  });

  it('falls back to a non-walk behavior outside Tauri', async () => {
    isTauriMock.mockReturnValue(false);
    vi.spyOn(Math, 'random').mockReturnValue(0.1); // walk range, but no Tauri
    const { container } = render(<PetRuntime pet={proceduralPet} />);
    // Pick at 6.8s; rest window 4.5s (ends 11.3s) — check inside it.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(7_000);
    });
    // r=0.1 < 0.62 → rest branch when walking is unavailable.
    expect(currentState(container)).toBe('rest');
    expect(petMoveBy).not.toHaveBeenCalled();
  });
});
