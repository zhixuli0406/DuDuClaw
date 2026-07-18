import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import {
  isDisplayableGlyph,
  glyphText,
  glyphIconFor,
  AgentGlyph,
} from './agent-glyph';

describe('isDisplayableGlyph', () => {
  it('treats emoji / pictographs as displayable', () => {
    expect(isDisplayableGlyph('🤖')).toBe(true);
    expect(isDisplayableGlyph('🐾')).toBe(true);
    expect(isDisplayableGlyph('🛡️')).toBe(true);
  });

  it('treats CJK glyphs as displayable', () => {
    expect(isDisplayableGlyph('客')).toBe(true);
  });

  it('rejects ASCII icon tokens (they must never render as text)', () => {
    expect(isDisplayableGlyph('briefcase')).toBe(false);
    expect(isDisplayableGlyph('shield-check')).toBe(false);
    expect(isDisplayableGlyph('git-branch')).toBe(false);
  });

  it('rejects empty / whitespace / nullish', () => {
    expect(isDisplayableGlyph('')).toBe(false);
    expect(isDisplayableGlyph('   ')).toBe(false);
    expect(isDisplayableGlyph(null)).toBe(false);
    expect(isDisplayableGlyph(undefined)).toBe(false);
  });
});

describe('glyphText', () => {
  it('returns the emoji unchanged', () => {
    expect(glyphText('🤖')).toBe('🤖');
  });

  it('returns the fallback (never the raw token) for an icon token', () => {
    expect(glyphText('briefcase')).toBe('🤖');
    expect(glyphText('shield-check', '🐾')).toBe('🐾');
  });

  it('returns the fallback for empty input', () => {
    expect(glyphText('')).toBe('🤖');
    expect(glyphText(null)).toBe('🤖');
  });
});

describe('glyphIconFor', () => {
  it('resolves a known lucide token to a component', () => {
    expect(glyphIconFor('briefcase')).toBeTruthy();
    expect(glyphIconFor('shield-check')).toBeTruthy();
    expect(glyphIconFor('fork-knife')).toBeTruthy(); // aliased to Utensils
  });

  it('is case-insensitive and trims', () => {
    expect(glyphIconFor('  Briefcase  ')).toBeTruthy();
  });

  it('returns null for emoji or unknown tokens', () => {
    expect(glyphIconFor('🤖')).toBeNull();
    expect(glyphIconFor('totally-unknown-token')).toBeNull();
    expect(glyphIconFor('')).toBeNull();
  });
});

describe('AgentGlyph', () => {
  it('renders an emoji as text', () => {
    const { container } = render(<AgentGlyph icon="🤖" />);
    expect(container.textContent).toContain('🤖');
    expect(container.querySelector('svg')).toBeNull();
  });

  it('renders a known token as a lucide icon, never the raw token text', () => {
    const { container } = render(<AgentGlyph icon="briefcase" />);
    expect(container.querySelector('svg')).not.toBeNull();
    expect(container.textContent).not.toContain('briefcase');
  });

  it('falls back to the brand glyph for an unknown token', () => {
    const { container } = render(<AgentGlyph icon="mystery-token" fallback="🐾" />);
    expect(container.querySelector('svg')).toBeNull();
    expect(container.textContent).toContain('🐾');
    expect(container.textContent).not.toContain('mystery-token');
  });
});
