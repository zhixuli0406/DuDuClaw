import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import type { ModelReview } from '@/lib/causal-api';
import { CausalDagView } from './CausalDagView';

function renderDag(model: ModelReview, onSelectEdge: (edge: ModelReview['edges'][number]) => void) {
  return render(
    <IntlProvider locale="en" messages={en} defaultLocale="en">
      <CausalDagView model={model} onSelectEdge={onSelectEdge} />
    </IntlProvider>,
  );
}

const review = {
  model: { id: 'model-1', name: 'Review model', version: 'v2', review_state: 'candidate', treatment_variable_id: 'a', outcome_variable_id: 'b' },
  effective_state: 'candidate',
  variables: [
    { id: 'a', name: 'A', version: 'v1', kind: 'binary' },
    { id: 'b', name: 'B', version: 'v1', kind: 'continuous' },
  ],
  edges: [{ id: 'claim-1', cause_variable: 'A', effect_variable: 'B', lag_min_seconds: 1, lag_max_seconds: 5,
    modality: 'asserted', context_json: '{}', review_state: 'candidate', reviewer: null, created_at: 1 }],
  active_opposition_count: 0, active_opposition_digest: '',
} as ModelReview;

describe('CausalDagView', () => {
  it('shows directed nodes and lets reviewers open the claim evidence', () => {
    const onSelectEdge = vi.fn();
    renderDag(review, onSelectEdge);
    expect(screen.getByRole('group', { name: 'Review model causal DAG' })).toBeInTheDocument();
    expect(screen.getByRole('group', { name: 'Variable A' })).toBeInTheDocument();
    expect(screen.getByRole('group', { name: 'Variable B' })).toBeInTheDocument();
    const edge = screen.getByRole('button', { name: /A leads to B/ });
    fireEvent.focus(edge);
    expect(screen.getByTestId('edge-focus-ring')).toHaveAttribute('stroke', 'currentColor');
    fireEvent.keyDown(edge, { key: 'Enter' });
    fireEvent.keyDown(edge, { key: ' ' });
    expect(onSelectEdge).toHaveBeenNthCalledWith(1, review.edges[0]);
    fireEvent.click(edge);
    expect(onSelectEdge).toHaveBeenCalledTimes(3);
    fireEvent.blur(edge);
    expect(screen.queryByTestId('edge-focus-ring')).not.toBeInTheDocument();
  });

  it('keeps a long node label available in full while fitting it into the node', () => {
    const longName = 'CustomerWaitDurationBeforeEscalation';
    const longReview = {
      ...review,
      variables: [{ id: 'long', name: longName, version: 'v1', kind: 'continuous' as const }],
      edges: [],
    };
    renderDag(longReview, vi.fn());
    const node = screen.getByRole('group', { name: `Variable ${longName}` });
    expect(node).toBeInTheDocument();
    expect(node.querySelector('title')?.textContent).toBe(longName);
    expect(node.querySelector('tspan')?.textContent).toBe('CustomerWa');
  });

  it('wraps full width labels within the node diameter', () => {
    const longName = '一二三四五六七八九十';
    const cjkReview = {
      ...review,
      variables: [{ id: 'long', name: longName, version: 'v1', kind: 'continuous' as const }],
      edges: [],
    };
    renderDag(cjkReview, vi.fn());
    const node = screen.getByRole('group', { name: `Variable ${longName}` });
    const lines = [...(node.querySelectorAll('tspan'))].map((line) => line.textContent ?? '');
    expect(lines).toEqual(['一二三四五六', '七八九十']);
  });

  it('treats supplementary CJK Extension B characters as full width', () => {
    const longName = '𠀀'.repeat(10);
    const extensionBReview = {
      ...review,
      variables: [{ id: 'ext-b', name: longName, version: 'v1', kind: 'continuous' as const }],
      edges: [],
    };
    renderDag(extensionBReview, vi.fn());
    const node = screen.getByRole('group', { name: `Variable ${longName}` });
    const lines = [...node.querySelectorAll('tspan')].map((line) => line.textContent ?? '');
    expect(lines).toEqual(['𠀀'.repeat(6), '𠀀'.repeat(4)]);
  });

  it('keeps a seven node chain laid out left to right with forward arrows', () => {
    const names = ['A', 'B', 'C', 'D', 'E', 'F', 'G'];
    const chainReview = {
      ...review,
      variables: names.map((name) => ({ id: name, name, version: 'v1', kind: 'continuous' as const })),
      edges: names.slice(0, -1).map((name, index) => ({
        ...review.edges[0], id: `edge-${name}`, cause_variable: name, effect_variable: names[index + 1],
      })),
    };
    renderDag(chainReview, vi.fn());
    const svg = screen.getByRole('group', { name: 'Review model causal DAG' });
    expect(Number(svg.getAttribute('viewBox')?.split(' ')[2])).toBeGreaterThan(720);
    for (const edge of screen.getAllByRole('button')) {
      const path = edge.querySelector('path')?.getAttribute('d') ?? '';
      const coordinates = path.match(/M ([\d.]+) ([\d.]+) L ([\d.]+) ([\d.]+)/);
      expect(coordinates).not.toBeNull();
      expect(Number(coordinates?.[3])).toBeGreaterThan(Number(coordinates?.[1]));
    }
  });
});
