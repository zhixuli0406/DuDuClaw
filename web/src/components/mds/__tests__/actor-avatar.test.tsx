import { describe, it, expect } from 'vitest';
import { fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { ActorAvatar } from '../actor-avatar';

describe('<ActorAvatar>', () => {
  it('renders a type-specific fallback glyph when there is no image', () => {
    const { container } = renderWithProviders(<ActorAvatar actorType="agent" />);
    const avatar = container.querySelector('[data-slot="actor-avatar"]')!;
    expect(avatar).toHaveAttribute('data-actor-type', 'agent');
    expect(
      container.querySelector('[data-slot="actor-avatar-fallback"]')
    ).toBeInTheDocument();
    expect(container.querySelector('img')).toBeNull();
  });

  it('falls back to the glyph when the image errors', () => {
    const { container } = renderWithProviders(
      <ActorAvatar actorType="squad" src="/broken.png" />
    );
    expect(container.querySelector('img')).toBeInTheDocument();
    fireEvent.error(container.querySelector('img')!);
    expect(container.querySelector('img')).toBeNull();
    expect(
      container.querySelector('[data-slot="actor-avatar-fallback"]')
    ).toBeInTheDocument();
  });

  it('applies the size-tier dimensions', () => {
    const { container } = renderWithProviders(
      <ActorAvatar actorType="user" size="2xl" />
    );
    expect(container.querySelector('[data-slot="actor-avatar"]')).toHaveClass(
      'size-14'
    );
  });

  it('shows a status dot colored by availability', () => {
    const { container } = renderWithProviders(
      <ActorAvatar actorType="agent" showStatusDot status="online" />
    );
    const dot = container.querySelector('[data-slot="actor-avatar-status"]')!;
    expect(dot).toHaveClass('bg-success', 'size-1.5', 'rounded-full');
    expect(dot).toHaveAttribute('data-status', 'online');
  });

  it('omits the status dot by default', () => {
    const { container } = renderWithProviders(<ActorAvatar actorType="system" />);
    expect(
      container.querySelector('[data-slot="actor-avatar-status"]')
    ).toBeNull();
  });
});
