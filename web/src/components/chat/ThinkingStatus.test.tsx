import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { applyThinking, startTurnThinking, type StepNode } from '@/stores/chat-store';
import { ThinkingStatus } from './ThinkingStatus';

function mockReducedMotion(reduce: boolean) {
  const mql = {
    matches: reduce,
    media: '(prefers-reduced-motion: reduce)',
    addEventListener: () => {},
    removeEventListener: () => {},
  };
  window.matchMedia = vi.fn().mockReturnValue(mql) as unknown as typeof window.matchMedia;
}

const T0 = 1_700_000_000_000;

describe('<ThinkingStatus>', () => {
  const originalMatchMedia = window.matchMedia;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(T0);
    mockReducedMotion(false);
  });

  afterEach(() => {
    vi.useRealTimers();
    window.matchMedia = originalMatchMedia;
  });

  it('counts live seconds from the send time, then switches to minutes', () => {
    renderWithProviders(<ThinkingStatus thinking={startTurnThinking(T0)} stepTree={[]} />);
    const secs = () => screen.getByTestId('thinking-status-seconds').textContent;
    expect(secs()).toBe('Thinking… 1s');

    act(() => {
      vi.advanceTimersByTime(1_100);
    });
    expect(secs()).toBe('Thinking… 2s');

    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    expect(secs()).toBe('Thinking… 12s');

    act(() => {
      vi.advanceTimersByTime(53_000);
    });
    expect(secs()).toBe('Thinking… 1m 5s');
  });

  it('is a polite status region that announces only a fixed label', () => {
    renderWithProviders(<ThinkingStatus thinking={startTurnThinking(T0)} stepTree={[]} />);
    const region = screen.getByRole('status');
    expect(region).toHaveAttribute('aria-live', 'polite');
    expect(region.querySelector('.sr-only')?.textContent).toBe('Thinking…');
    // The ticking number and progress line are hidden from assistive tech.
    expect(screen.getByTestId('thinking-status-visual')).toHaveAttribute('aria-hidden', 'true');
  });

  it('shows approximate tokens and the newest progress line', () => {
    let turn = startTurnThinking(T0);
    turn = applyThinking(turn, { phase: 'update', block: 't0', kind: 'summary', line: 'Reading the report', tokens: 1240 });
    renderWithProviders(<ThinkingStatus thinking={turn} stepTree={[]} />);
    expect(screen.getByText('~1.2k tokens')).toBeInTheDocument();
    expect(screen.getByTestId('thinking-status-line').textContent).toBe('Reading the report');
  });

  it('falls back to the running tool step as the progress line', () => {
    const tree: StepNode[] = [
      { id: 'a', tool: 'Read', summary: 'a.md', depth: 0, running: false, ts: T0 },
      { id: 'b', tool: 'Grep', summary: 'TODO', depth: 0, running: true, ts: T0 },
    ];
    renderWithProviders(<ThinkingStatus thinking={startTurnThinking(T0)} stepTree={tree} />);
    expect(screen.getByTestId('thinking-status-line').textContent).toBe('Grep · TODO');
  });

  it('under reduced motion the orb is static and the counter still ticks', () => {
    mockReducedMotion(true);
    const { container } = renderWithProviders(<ThinkingStatus thinking={startTurnThinking(T0)} stepTree={[]} />);
    expect(container.querySelector('canvas')).toBeNull();
    expect(container.querySelector('[data-thinking-state="static"]')).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(2_100);
    });
    expect(screen.getByTestId('thinking-status-seconds').textContent).toBe('Thinking… 3s');
  });
});
