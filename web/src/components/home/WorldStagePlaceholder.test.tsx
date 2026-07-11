import { describe, it, expect, afterEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { WorldStagePlaceholder } from './WorldStagePlaceholder';

const AGENTS = [
  { name: 'scout', display_name: 'Scout', status: 'active' as const },
  { name: 'ops', display_name: 'Ops', status: 'paused' as const },
];

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('<WorldStagePlaceholder> degradation chain', () => {
  it('falls back to the static scene when WebGL is unavailable (jsdom default)', () => {
    // jsdom has no WebGL context and no matchMedia → resolveStageMode = static.
    renderWithProviders(<WorldStagePlaceholder agents={AGENTS} />);
    // The static illustrated office is labelled; no <canvas> is mounted.
    expect(screen.getByLabelText('Office scene')).toBeInTheDocument();
    expect(document.querySelector('canvas')).toBeNull();
    // Busts for both agents are drawn.
    expect(screen.getByText('Scout')).toBeInTheDocument();
    expect(screen.getByText('Ops')).toBeInTheDocument();
  });

  it('hides the stage/list toggle when the stage cannot render at all', () => {
    // WebGL unavailable ⇒ stage is impossible ⇒ toggle is pointless and hidden.
    renderWithProviders(<WorldStagePlaceholder agents={AGENTS} />);
    expect(screen.queryByRole('button', { name: /list view|world view/i })).toBeNull();
  });

  it('stays static under prefers-reduced-motion (matchMedia mocked)', () => {
    vi.stubGlobal(
      'matchMedia',
      vi.fn((query: string) => ({
        matches: query.includes('reduced-motion'),
        media: query,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
        onchange: null,
      })),
    );
    renderWithProviders(<WorldStagePlaceholder agents={AGENTS} />);
    expect(screen.getByLabelText('Office scene')).toBeInTheDocument();
    expect(document.querySelector('canvas')).toBeNull();
  });
});
