import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { DuDu } from './DuDu';
import { DUDU_FACES, FACE_PRESETS, type DuduFace } from './faces';
import { VISEMES } from './visemes';

describe('DuDu', () => {
  it.each(DUDU_FACES)('renders the %s face preset without crashing', (face) => {
    const { container } = render(<DuDu face={face} idPrefix={`t-${face}`} />);
    const svg = container.querySelector('svg[data-face]');
    expect(svg).not.toBeNull();
    expect(svg!.getAttribute('data-face')).toBe(face);
  });

  it('exposes exactly the 13 documented presets', () => {
    expect(DUDU_FACES).toHaveLength(13);
    for (const face of DUDU_FACES) {
      expect(FACE_PRESETS[face]).toBeDefined();
    }
  });

  it('renders eyebrows only for presets that declare showBrows', () => {
    for (const face of DUDU_FACES) {
      const { container } = render(<DuDu face={face} idPrefix={`b-${face}`} />);
      const brows = container.querySelector(`g[data-face-brows="${face}"]`);
      if (FACE_PRESETS[face].showBrows) {
        expect(brows, `${face} should show brows`).not.toBeNull();
      } else {
        expect(brows, `${face} should hide brows`).toBeNull();
      }
    }
  });

  it('drives a viseme-distinct mouth while speaking', () => {
    const { container: open } = render(<DuDu face="speaking" viseme={VISEMES.A} idPrefix="sp-a" />);
    const { container: rest } = render(<DuDu face="speaking" viseme={VISEMES.M} idPrefix="sp-m" />);
    const openMouth = open.querySelector('path[data-face="speaking"]')?.getAttribute('d');
    const restMouth = rest.querySelector('path[data-face="speaking"]')?.getAttribute('d');
    expect(openMouth).toBeTruthy();
    expect(restMouth).toBeTruthy();
    // An open "A" mouth must differ from the closed "M" mouth.
    expect(openMouth).not.toBe(restMouth);
  });

  it('maps size keywords to pixel dimensions', () => {
    const cases: Array<['sm' | 'md' | 'lg', string]> = [
      ['sm', '48'],
      ['md', '96'],
      ['lg', '160'],
    ];
    for (const [kw, pxStr] of cases) {
      const { container } = render(<DuDu face="idle" size={kw} idPrefix={`sz-${kw}`} />);
      const svg = container.querySelector('svg');
      expect(svg?.getAttribute('width')).toBe(pxStr);
      expect(svg?.getAttribute('height')).toBe(pxStr);
    }
  });

  it('accepts an explicit numeric size', () => {
    const { container } = render(<DuDu face="idle" size={200} idPrefix="sz-num" />);
    expect(container.querySelector('svg')?.getAttribute('width')).toBe('200');
  });

  it('closes the eyes (no catchlights) when sleeping', () => {
    const { container } = render(<DuDu face="sleep" idPrefix="slp" />);
    // Catchlights are cream circles r=1.3 painted over open eyes — absent when asleep.
    const catchlights = Array.from(container.querySelectorAll('circle')).filter(
      (c) => c.getAttribute('r') === '1.3',
    );
    expect(catchlights).toHaveLength(0);
  });

  it('sets an accessible label per face', () => {
    const face: DuduFace = 'celebrating';
    const { getByRole } = render(<DuDu face={face} idPrefix="a11y" />);
    expect(getByRole('img').getAttribute('aria-label')).toBe('DuDu, celebrating');
  });
});
