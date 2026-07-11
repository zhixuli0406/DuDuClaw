import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { CharacterAvatar } from './CharacterAvatar';
import { StatusEmote } from './StatusEmote';
import { agentPose, agentEmote } from './poses';

describe('<CharacterAvatar>', () => {
  it('renders an accessible image labelled by name', () => {
    render(<CharacterAvatar agentId="scout" name="Scout" />);
    expect(screen.getByRole('img', { name: 'Scout' })).toBeInTheDocument();
  });

  it('falls back to the agent id when no name is given', () => {
    render(<CharacterAvatar agentId="ops-bot" />);
    expect(screen.getByRole('img', { name: 'ops-bot' })).toBeInTheDocument();
  });

  it('renders an svg face', () => {
    const { container } = render(<CharacterAvatar agentId="a1" name="A" />);
    expect(container.querySelector('svg')).toBeTruthy();
    // A gradient def is always present (the tint).
    expect(container.querySelector('linearGradient')).toBeTruthy();
  });

  it('renders the bust variant when size is large', () => {
    const { container } = render(<CharacterAvatar agentId="a1" name="A" size={112} />);
    // Bust body path (mound) present.
    expect(container.querySelector('path[d^="M6 48"]')).toBeTruthy();
  });

  it('renders a head-top emote when provided', () => {
    render(<CharacterAvatar agentId="a1" name="A" emote="working" />);
    expect(screen.getByRole('img', { name: 'Working' })).toBeInTheDocument();
  });

  it('renders a live dot when live', () => {
    const { container } = render(<CharacterAvatar agentId="a1" name="A" live />);
    expect(container.querySelector('.animate-ping')).toBeTruthy();
  });

  it('applies the blink animation class only when animated and not sleeping', () => {
    const { container: on } = render(<CharacterAvatar agentId="a1" name="A" animated />);
    expect(on.querySelector('.character-eyes')).toBeTruthy();

    const { container: sleeping } = render(
      <CharacterAvatar agentId="a1" name="A" pose="sleeping" animated />,
    );
    expect(sleeping.querySelector('.character-eyes')).toBeNull();

    const { container: still } = render(
      <CharacterAvatar agentId="a1" name="A" animated={false} />,
    );
    expect(still.querySelector('.character-eyes')).toBeNull();
  });
});

describe('<StatusEmote>', () => {
  it('labels the bubble by state', () => {
    render(<StatusEmote kind="blocked" />);
    expect(screen.getByRole('img', { name: 'Blocked' })).toBeInTheDocument();
  });
});

describe('pose mapping', () => {
  it('maps agent lifecycle to a pose', () => {
    expect(agentPose('active', true)).toBe('working');
    expect(agentPose('active', false)).toBe('idle');
    expect(agentPose('paused')).toBe('sleeping');
    expect(agentPose('terminated')).toBe('sleeping');
  });

  it('maps agent lifecycle to an emote (or none)', () => {
    expect(agentEmote('active', true)).toBe('working');
    expect(agentEmote('active', false)).toBeNull();
    expect(agentEmote('paused')).toBe('sleeping');
  });
});
