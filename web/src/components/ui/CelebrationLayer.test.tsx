import { describe, it, expect, vi, afterEach } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import { toastBus } from '@/lib/toast';
import { CelebrationLayer, celebrate } from './CelebrationLayer';

describe('CelebrationLayer', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders a badge burst when motion is allowed', () => {
    render(<CelebrationLayer />);
    act(() => {
      celebrate('badge', { message: '解鎖成就' });
    });
    expect(screen.getByText('🏆')).toBeInTheDocument();
  });

  it('emits confetti pieces for a confetti burst', () => {
    render(<CelebrationLayer />);
    act(() => {
      celebrate('confetti');
    });
    expect(document.querySelectorAll('.confetti-piece').length).toBeGreaterThan(0);
  });

  it('reduced-motion: no particles, degrades to a toast', () => {
    vi.stubGlobal('matchMedia', () => ({ matches: true }) as MediaQueryList);
    const seen: string[] = [];
    const unsub = toastBus.subscribe((t) => seen.push(t.message));

    render(<CelebrationLayer />);
    act(() => {
      celebrate('confetti', { message: '收件匣清空了' });
    });

    expect(document.querySelectorAll('.confetti-piece').length).toBe(0);
    expect(seen).toContain('收件匣清空了');
    unsub();
  });
});
