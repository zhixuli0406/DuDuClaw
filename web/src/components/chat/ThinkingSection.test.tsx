import { describe, it, expect } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import {
  applyThinking,
  finishTurnThinking,
  startTurnThinking,
  type ChatMessage,
  type ThinkingFrame,
  type TurnThinking,
} from '@/stores/chat-store';
import { ThinkingSection } from './ThinkingSection';
import { TaskInsights } from './TaskInsights';
import { splitTurnMessages } from './turn-layout';

const T0 = 1_700_000_000_000;

function turn(frames: ThinkingFrame[], ms = 8_200, meta?: { tokens?: number }): TurnThinking {
  return finishTurnThinking(frames.reduce(applyThinking, startTurnThinking(T0)), T0 + ms, meta);
}

describe('<ThinkingSection>', () => {
  it('shows the wall time and approximate tokens, and expands to labelled text', () => {
    const t = turn([
      { phase: 'end', block: 't0', kind: 'summary', text: 'Compared both reports.', duration_ms: 3_000 },
      { phase: 'end', block: 't1', kind: 'raw', text: 'raw notes', truncated: true },
      { phase: 'end', block: 'turn', kind: 'progress', tokens: 1240 },
    ]);
    renderWithProviders(<ThinkingSection thinking={t} />);
    const toggle = screen.getByRole('button', { name: /Thought for 9s/ });
    expect(toggle).toHaveTextContent('~1.2k tokens');
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('Compared both reports.')).toBeNull();

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('Thinking summary')).toBeInTheDocument();
    expect(screen.getByText('Compared both reports.')).toBeInTheDocument();
    expect(screen.getByText('3s')).toBeInTheDocument();
    expect(screen.getByText('Raw reasoning (secrets masked)')).toBeInTheDocument();
    expect(screen.getByText('(truncated)')).toBeInTheDocument();
  });

  it('formats minutes for long turns', () => {
    renderWithProviders(<ThinkingSection thinking={turn([], 125_000)} />);
    expect(screen.getByTestId('thinking-section')).toHaveTextContent('Thought for 2m 5s');
  });

  it('omitted: no chevron, not a button, and a fixed note', () => {
    renderWithProviders(<ThinkingSection thinking={turn([{ phase: 'end', block: 't0', kind: 'omitted', tokens: 300 }])} />);
    expect(screen.queryByTestId('thinking-chevron')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.getByText("This model doesn't provide a thinking summary")).toBeInTheDocument();
    expect(screen.getByTestId('thinking-section')).toHaveTextContent('~300 tokens');
  });

  it('redacted: the text already shown is gone and a fixed note replaces it', () => {
    const t = turn([
      { phase: 'update', block: 't0', kind: 'summary', text: 'sk-like text' },
      { phase: 'end', block: 't0', kind: 'redacted' },
    ]);
    renderWithProviders(<ThinkingSection thinking={t} />);
    expect(screen.queryByText('sk-like text')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.getByText('Some thinking was withheld by safety rules')).toBeInTheDocument();
  });

  it('raw reasoning withheld because redaction is off shows the hint', () => {
    renderWithProviders(
      <ThinkingSection thinking={turn([{ phase: 'end', block: 't0', kind: 'raw', raw_needs_redaction: true }])} />,
    );
    expect(screen.getByText('Enable redaction to show raw reasoning from open models')).toBeInTheDocument();
  });

  it('no provider thinking at all: just the wall time', () => {
    renderWithProviders(<ThinkingSection thinking={turn([], 2_400)} />);
    const section = screen.getByTestId('thinking-section');
    expect(section).toHaveTextContent('Thought for 3s');
    expect(section).not.toHaveTextContent('tokens');
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('compact form moves notes out of the visible row', () => {
    renderWithProviders(
      <ThinkingSection thinking={turn([{ phase: 'end', block: 't0', kind: 'omitted' }])} compact />,
    );
    const note = "This model doesn't provide a thinking summary";
    expect(screen.getByText(note)).toHaveClass('sr-only');
  });
});

describe('<TaskInsights> with thinking', () => {
  it('renders with only thinking (no steps) and no step counter', () => {
    renderWithProviders(<TaskInsights tree={[]} todos={[]} streaming={false} thinking={turn([], 4_000)} />);
    expect(screen.getByTestId('thinking-section')).toHaveTextContent('Thought for 4s');
    expect(screen.queryByText('Task insights')).toBeNull();
  });

  it('still renders nothing with neither steps nor thinking', () => {
    const { container } = renderWithProviders(<TaskInsights tree={[]} todos={[]} streaming={false} />);
    expect(container.firstChild).toBeNull();
  });

  it('heads the step tree with the thinking line', () => {
    renderWithProviders(
      <TaskInsights
        tree={[{ id: 'a', tool: 'Read', summary: 'a.md', depth: 0, running: false, ts: T0 }]}
        todos={[]}
        streaming={false}
        thinking={turn([], 1_000)}
      />,
    );
    expect(screen.getByTestId('thinking-section')).toHaveTextContent('Thought for 1s');
    expect(screen.getByText('Task insights')).toBeInTheDocument();
  });
});

describe('splitTurnMessages', () => {
  const user: ChatMessage = { id: 'u', role: 'user', content: 'q', timestamp: T0 };
  const reply: ChatMessage = { id: 'a', role: 'assistant', content: 'a', timestamp: T0, tokens: 1, thinking: turn([]) };

  it('splits off the finished reply that carries thinking', () => {
    const r = splitTurnMessages([user, reply], false);
    expect(r.turnReply).toBe(reply);
    expect(r.prior).toEqual([user]);
  });

  it('leaves history replies without thinking in place', () => {
    const plain: ChatMessage = { ...reply, thinking: undefined };
    const r = splitTurnMessages([user, plain], false);
    expect(r.turnReply).toBeNull();
    expect(r.prior).toEqual([user, plain]);
  });

  it('splits off a still-streaming reply', () => {
    const streaming: ChatMessage = { id: 's', role: 'assistant', content: 'par', timestamp: T0 };
    const r = splitTurnMessages([user, streaming], true);
    expect(r.streamingAssistant).toBe(streaming);
    expect(r.turnReply).toBeNull();
  });
});
