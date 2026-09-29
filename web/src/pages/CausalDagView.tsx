import { useState, type KeyboardEvent } from 'react';
import { useIntl } from 'react-intl';
import type { CausalClaim, ModelReview } from '@/lib/causal-api';

interface CausalDagViewProps {
  model: ModelReview;
  onSelectEdge: (edge: CausalClaim) => void;
}

function nodeLayout(model: ModelReview) {
  const names = new Set(model.variables.map((variable) => variable.name));
  for (const edge of model.edges) {
    names.add(edge.cause_variable);
    names.add(edge.effect_variable);
  }
  const ordered = [...names].sort((a, b) => a.localeCompare(b));
  const outgoing = new Map(ordered.map((name) => [name, [] as string[]]));
  const incoming = new Map(ordered.map((name) => [name, 0]));
  for (const edge of model.edges) {
    outgoing.get(edge.cause_variable)?.push(edge.effect_variable);
    incoming.set(edge.effect_variable, (incoming.get(edge.effect_variable) ?? 0) + 1);
  }

  const queue = ordered.filter((name) => incoming.get(name) === 0);
  const ranks = new Map(queue.map((name) => [name, 0]));
  for (let index = 0; index < queue.length; index += 1) {
    const name = queue[index];
    for (const child of outgoing.get(name) ?? []) {
      ranks.set(child, Math.max(ranks.get(child) ?? 0, (ranks.get(name) ?? 0) + 1));
      incoming.set(child, (incoming.get(child) ?? 0) - 1);
      if (incoming.get(child) === 0) queue.push(child);
    }
  }
  // If the graph contains a cycle, keep it readable in a single row. The page
  // still reports the cycle separately and every stored edge remains visible.
  const hasCycle = ranks.size < ordered.length;
  if (hasCycle) ordered.forEach((name, index) => ranks.set(name, index));

  const byRank = new Map<number, string[]>();
  for (const name of ordered) {
    const rank = ranks.get(name) ?? 0;
    byRank.set(rank, [...(byRank.get(rank) ?? []), name]);
  }
  const maxRank = Math.max(0, ...byRank.keys());
  const rankSpacing = 144;
  const width = Math.max(320, 184 + maxRank * rankSpacing);
  const height = Math.max(160, ...[...byRank.values()].map((items) => items.length * 86 + 48));
  const positions = new Map<string, { x: number; y: number }>();
  for (const [rank, items] of byRank) {
    items.forEach((name, index) => positions.set(name, {
      x: 92 + rank * rankSpacing,
      y: ((index + 1) * height) / (items.length + 1),
    }));
  }
  return { positions, width, height };
}

function edgeKeyEvent(event: KeyboardEvent<SVGGElement>, edge: CausalClaim, select: (edge: CausalClaim) => void) {
  if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault();
    select(edge);
  }
}

function estimatedCharacterWidth(character: string): number {
  const codePoint = character.codePointAt(0) ?? 0;
  const eastAsianWideRanges: Array<[number, number]> = [
    [0x1100, 0x11ff], [0x2e80, 0x303e], [0x3040, 0xa4cf],
    [0xac00, 0xd7af], [0xf900, 0xfaff], [0xfe10, 0xfe6f],
    [0xff01, 0xff60], [0xffe0, 0xffe6], [0x1b000, 0x1b2ff],
    [0x20000, 0x2fa1f], [0x30000, 0x3fffd],
  ];
  if (eastAsianWideRanges.some(([start, end]) => codePoint >= start && codePoint <= end)) return 12;
  if (/\s/u.test(character)) return 4;
  if (/[ilI.,'!|:;]/u.test(character)) return 4;
  if (/[MW@#%]/u.test(character)) return 10;
  return 7;
}

function nodeLines(name: string): [string, string?] {
  const lines = ['', ''];
  const widths = [0, 0];
  const maxLineWidth = 76;
  let lineIndex = 0;
  for (const character of Array.from(name)) {
    const characterWidth = estimatedCharacterWidth(character);
    if (widths[lineIndex] + characterWidth <= maxLineWidth) {
      lines[lineIndex] += character;
      widths[lineIndex] += characterWidth;
      continue;
    }
    if (lineIndex === 0) {
      lineIndex = 1;
      lines[lineIndex] += character;
      widths[lineIndex] += characterWidth;
      continue;
    }
    while (lines[1] && widths[1] + 8 > maxLineWidth) {
      const last = Array.from(lines[1]).at(-1)!;
      lines[1] = Array.from(lines[1]).slice(0, -1).join('');
      widths[1] -= estimatedCharacterWidth(last);
    }
    lines[1] += '…';
    break;
  }
  return [lines[0], lines[1] || undefined];
}

export function CausalDagView({ model, onSelectEdge }: CausalDagViewProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const { positions, width, height } = nodeLayout(model);
  const nodes = [...positions.entries()];
  const [focusedEdgeId, setFocusedEdgeId] = useState<string | null>(null);
  return (
    <div className="overflow-x-auto rounded-md border bg-muted/20 p-2">
      <svg className="block max-w-none" style={{ width: `${width}px`, minWidth: '100%' }} viewBox={`0 0 ${width} ${height}`} role="group" aria-label={t('causalCuration.dag.ariaLabel', { name: model.model.name })}>
        <defs>
          <marker id={`arrow-${model.model.id}`} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" className="fill-current text-muted-foreground" />
          </marker>
        </defs>
        {model.edges.map((edge) => {
          const from = positions.get(edge.cause_variable);
          const to = positions.get(edge.effect_variable);
          if (!from || !to) return null;
          const sameRank = from.x === to.x;
          const path = sameRank
            ? `M ${from.x + 48} ${from.y} C ${from.x + 112} ${from.y - 38}, ${to.x + 112} ${to.y - 38}, ${to.x + 48} ${to.y}`
            : from.x < to.x
              ? `M ${from.x + 48} ${from.y} L ${to.x - 48} ${to.y}`
              : `M ${from.x - 48} ${from.y} L ${to.x + 48} ${to.y}`;
          const label = t('causalCuration.dag.edgeAriaLabel', {
            cause: edge.cause_variable, effect: edge.effect_variable, state: edge.review_state,
            lagMin: edge.lag_min_seconds, lagMax: edge.lag_max_seconds,
          });
          return <g key={edge.id} role="button" tabIndex={0} aria-label={label} onClick={() => onSelectEdge(edge)}
            onFocus={() => setFocusedEdgeId(edge.id)} onBlur={() => setFocusedEdgeId(null)}
            onKeyDown={(event) => edgeKeyEvent(event, edge, onSelectEdge)} className="group cursor-pointer text-muted-foreground focus-visible:outline-none">
            <path d={path} fill="none" stroke="currentColor" strokeWidth="2" markerEnd={`url(#arrow-${model.model.id})`} />
            <path d={path} fill="none" stroke="transparent" strokeWidth="16" />
            {focusedEdgeId === edge.id && <path data-testid="edge-focus-ring" d={path} fill="none" stroke="currentColor" strokeDasharray="5 4" strokeWidth="7" opacity="0.75" pointerEvents="none" />}
            <text x={(from.x + to.x) / 2} y={(from.y + to.y) / 2 - 7} textAnchor="middle" className="fill-current text-[10px]">
              {edge.review_state} · {edge.modality}
            </text>
          </g>;
        })}
        {nodes.map(([name, point]) => {
          const [firstLine, secondLine] = nodeLines(name);
          return <g key={name} role="group" aria-label={t('causalCuration.dag.variableAriaLabel', { name })}>
          <title>{name}</title>
          <circle cx={point.x} cy={point.y} r="46" className="fill-background stroke-brand" strokeWidth="2" />
          <text x={point.x} y={point.y + (secondLine ? -3 : 4)} textAnchor="middle" className="fill-current text-xs font-medium">
            <tspan x={point.x} dy="0">{firstLine}</tspan>
            {secondLine && <tspan x={point.x} dy="14">{secondLine}</tspan>}
          </text>
        </g>;
        })}
        {model.edges.length === 0 && <text x="360" y={height / 2} textAnchor="middle" className="fill-current text-sm text-muted-foreground">{t('causalCuration.dag.noEdges')}</text>}
      </svg>
    </div>
  );
}
