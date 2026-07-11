import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { SpeechBubble } from './SpeechBubble';

describe('<SpeechBubble>', () => {
  it('renders its message', () => {
    renderWithProviders(<SpeechBubble>上工了 🐾</SpeechBubble>);
    expect(screen.getByText('上工了 🐾')).toBeInTheDocument();
  });
});
