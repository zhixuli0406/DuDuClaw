import { useRef, useEffect, useMemo } from 'react';
import { useIntl } from 'react-intl';
import * as d3 from 'd3';
import type { WikiPageMeta, MemoryGraphEdge } from '@/lib/api';
import {
  buildTopicGraph,
  topicPrimarySource,
  type TopicGraphNode,
  type LinkKind,
  type TopicKind,
} from '@/lib/topic-graph';

/**
 * WikiGraph — the Knowledge Hub 關聯圖 (WP16).
 *
 * Topic-oriented, not document-oriented: knowledge-base pages rarely cross-link,
 * so a link-only graph rendered as a single lonely dot. Here the topics a page
 * talks about (tags, its directory, and the entities the AI actually learned in
 * `memory.graph`) are nodes in their own right, and pages hang off them. All
 * assembly rules live in `@/lib/topic-graph` — this file only draws.
 *
 * Palette is intentionally NOT tokenised (web/DESIGN.md §5.1 — d3 graphs carry
 * their own node/edge colours, as MemoryGraph does).
 */

interface GNode extends d3.SimulationNodeDatum, TopicGraphNode {}

interface GLink extends d3.SimulationLinkDatum<GNode> {
  source: string | GNode;
  target: string | GNode;
  kind: LinkKind;
  label?: string;
}

const PAGE_COLOR = '#f59e0b'; // amber-500 — a page
const TOPIC_COLOR: Record<TopicKind, string> = {
  tag: '#8b5cf6', // violet-500 — an explicit tag
  category: '#3b82f6', // blue-500 — the page's folder
  entity: '#10b981', // emerald-500 — something the AI learned
};

const LINK_STYLE: Record<LinkKind, { stroke: string; opacity: number; width: number; dash?: string }> = {
  'page-topic': { stroke: '#a8a29e', opacity: 0.35, width: 1 },
  'page-page-link': { stroke: '#f59e0b', opacity: 0.6, width: 1.8 },
  'page-page-shared': { stroke: '#a8a29e', opacity: 0.3, width: 1, dash: '4 3' },
  'topic-topic': { stroke: '#8b5cf6', opacity: 0.4, width: 1.4 },
};

function nodeColor(n: TopicGraphNode): string {
  return n.kind === 'page' ? PAGE_COLOR : TOPIC_COLOR[topicPrimarySource(n)];
}

function nodeRadius(n: TopicGraphNode): number {
  return (n.kind === 'page' ? 6 : 5) + Math.min(n.degree, 8) * 1.6;
}

function truncateLabel(s: string, max: number): string {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

interface WikiGraphProps {
  pages: ReadonlyArray<WikiPageMeta>;
  /** Raw page contents keyed by path, used to mine explicit references. */
  pageContents: Record<string, string>;
  /**
   * SPO triples from `memory.graph`. Optional — without them the graph still
   * works off tags and directories, just with fewer learned-topic nodes.
   */
  memoryEdges?: ReadonlyArray<MemoryGraphEdge>;
  width?: number;
  height?: number;
  onSelectPage?: (path: string) => void;
}

export function WikiGraph({
  pages,
  pageContents,
  memoryEdges,
  width = 700,
  height = 500,
  onSelectPage,
}: WikiGraphProps) {
  const intl = useIntl();
  const svgRef = useRef<SVGSVGElement>(null);
  // Stable ref for the callback so the d3 effect doesn't re-run per render.
  const onSelectPageRef = useRef(onSelectPage);
  useEffect(() => {
    onSelectPageRef.current = onSelectPage;
  }, [onSelectPage]);

  const uncategorized = intl.formatMessage({ id: 'wiki.graph.uncategorized' });

  const { nodes, links, stats } = useMemo(
    () =>
      buildTopicGraph(
        {
          pages: pages.map((p) => ({ path: p.path, title: p.title, tags: p.tags ?? [] })),
          pageContents,
          memoryEdges: memoryEdges?.map((e) => ({
            subject: e.subject,
            predicate: e.predicate,
            object: e.object,
          })),
        },
        { uncategorizedLabel: uncategorized },
      ),
    [pages, pageContents, memoryEdges, uncategorized],
  );

  useEffect(() => {
    if (!svgRef.current || nodes.length === 0) return;

    const reduceMotion =
      typeof window !== 'undefined' &&
      window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;

    const svg = d3.select(svgRef.current);
    svg.selectAll('*').remove();
    const g = svg.append('g');

    const zoom = d3
      .zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.3, 4])
      .on('zoom', (event) => g.attr('transform', event.transform));
    svg.call(zoom);

    // d3 mutates x/y/vx/vy in place — work on copies.
    const simNodes: GNode[] = nodes.map((n) => ({ ...n }));
    const simLinks: GLink[] = links.map((l) => ({ ...l }));

    const simulation = d3
      .forceSimulation<GNode>(simNodes)
      .force(
        'link',
        d3
          .forceLink<GNode, GLink>(simLinks)
          .id((d) => d.id)
          // Topics act as hubs: keep their spokes short so clusters read as clusters.
          .distance((l) => (l.kind === 'page-topic' ? 70 : 110)),
      )
      .force('charge', d3.forceManyBody().strength(-240))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force(
        'collision',
        d3.forceCollide<GNode>().radius((d) => nodeRadius(d) + 12),
      );

    const link = g
      .append('g')
      .selectAll('line')
      .data(simLinks)
      .join('line')
      .attr('stroke', (d) => LINK_STYLE[d.kind].stroke)
      .attr('stroke-opacity', (d) => LINK_STYLE[d.kind].opacity)
      .attr('stroke-width', (d) => LINK_STYLE[d.kind].width)
      .attr('stroke-dasharray', (d) => LINK_STYLE[d.kind].dash ?? null);
    link.append('title').text((d) => (d.label ? d.label : ''));

    const drag = d3
      .drag<SVGElement, GNode>()
      .on('start', (event, d) => {
        if (!event.active && !reduceMotion) simulation.alphaTarget(0.3).restart();
        d.fx = d.x;
        d.fy = d.y;
      })
      .on('drag', (event, d) => {
        d.fx = event.x;
        d.fy = event.y;
      })
      .on('end', (event, d) => {
        if (!event.active) simulation.alphaTarget(0);
        d.fx = null;
        d.fy = null;
      });

    // Pages are circles; topics are rounded squares — shape, not just colour,
    // separates the two kinds (readable without colour vision).
    const pageNodes = simNodes.filter((n) => n.kind === 'page');
    const topicNodes = simNodes.filter((n) => n.kind === 'topic');

    const pageSel = g
      .append('g')
      .selectAll<SVGCircleElement, GNode>('circle')
      .data(pageNodes)
      .join('circle')
      .attr('r', nodeRadius)
      .attr('fill', PAGE_COLOR)
      .attr('stroke', '#fafaf9')
      .attr('stroke-width', 1.5)
      .attr('cursor', 'pointer')
      .attr('data-node-kind', 'page')
      .on('click', (_event, d) => {
        if (d.path) onSelectPageRef.current?.(d.path);
      })
      .call(drag as unknown as (sel: d3.Selection<SVGCircleElement, GNode, SVGGElement, unknown>) => void);
    pageSel.append('title').text((d) => `${d.label}\n${d.path ?? ''}`);

    const topicSel = g
      .append('g')
      .selectAll<SVGRectElement, GNode>('rect')
      .data(topicNodes)
      .join('rect')
      .attr('width', (d) => nodeRadius(d) * 1.8)
      .attr('height', (d) => nodeRadius(d) * 1.8)
      .attr('rx', 3)
      .attr('fill', (d) => nodeColor(d))
      .attr('stroke', '#fafaf9')
      .attr('stroke-width', 1.5)
      .attr('cursor', 'grab')
      .attr('data-node-kind', 'topic')
      .call(drag as unknown as (sel: d3.Selection<SVGRectElement, GNode, SVGGElement, unknown>) => void);
    topicSel
      .append('title')
      .text((d) => `${d.label} · ${intl.formatMessage({ id: `wiki.graph.legend.${topicPrimarySource(d)}` })}`);

    const label = g
      .append('g')
      .selectAll('text')
      .data(simNodes)
      .join('text')
      .text((d) => truncateLabel(d.label, d.kind === 'page' ? 18 : 14))
      .attr('font-size', (d) => (d.kind === 'page' ? 10 : 10.5))
      .attr('font-weight', (d) => (d.kind === 'topic' ? 600 : 400))
      .attr('dx', (d) => nodeRadius(d) + 4)
      .attr('dy', 3)
      .attr('fill', 'currentColor')
      .attr('opacity', (d) => (d.kind === 'topic' ? 0.85 : 0.6))
      .attr('pointer-events', 'none');

    const tick = () => {
      const nx = (n: string | GNode) => (typeof n === 'object' ? (n.x ?? 0) : 0);
      const ny = (n: string | GNode) => (typeof n === 'object' ? (n.y ?? 0) : 0);
      link
        .attr('x1', (d) => nx(d.source))
        .attr('y1', (d) => ny(d.source))
        .attr('x2', (d) => nx(d.target))
        .attr('y2', (d) => ny(d.target));
      pageSel.attr('cx', (d) => d.x ?? 0).attr('cy', (d) => d.y ?? 0);
      topicSel
        .attr('x', (d) => (d.x ?? 0) - nodeRadius(d) * 0.9)
        .attr('y', (d) => (d.y ?? 0) - nodeRadius(d) * 0.9);
      label.attr('x', (d) => d.x ?? 0).attr('y', (d) => d.y ?? 0);
    };

    if (reduceMotion) {
      simulation.stop();
      simulation.tick(200);
      tick();
    } else {
      simulation.on('tick', tick);
    }

    return () => {
      simulation.stop();
      svg.on('.zoom', null);
      svg.selectAll('*').remove();
    };
    // `intl` is stable per locale; `uncategorized` already gates the rebuild.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodes, links, width, height]);

  if (nodes.length === 0) {
    return (
      <div className="flex items-center justify-center rounded-xl border border-dashed border-surface-border bg-surface py-12">
        <p className="text-muted-foreground">{intl.formatMessage({ id: 'wiki.graph.empty' })}</p>
      </div>
    );
  }

  const legend: Array<{ kind: 'page' | TopicKind; color: string }> = [
    { kind: 'page', color: PAGE_COLOR },
    { kind: 'tag', color: TOPIC_COLOR.tag },
    { kind: 'category', color: TOPIC_COLOR.category },
    { kind: 'entity', color: TOPIC_COLOR.entity },
  ];

  return (
    <div className="overflow-hidden rounded-xl border border-surface-border bg-surface shadow-[var(--surface-shadow)]">
      {/* Counts on the left, what the colours mean on the right. */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-surface-border px-4 py-2">
        <span className="text-xs font-medium tabular-nums text-muted-foreground">
          {intl.formatMessage({ id: 'wiki.graph.stats' }, stats)}
        </span>
        <span className="hidden h-3 w-px bg-surface-border sm:inline-block" />
        {legend.map(({ kind, color }) => (
          <div key={kind} className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span
              aria-hidden
              className={kind === 'page' ? 'inline-block size-2.5 rounded-full' : 'inline-block size-2.5 rounded-[2px]'}
              style={{ backgroundColor: color }}
            />
            {intl.formatMessage({ id: `wiki.graph.legend.${kind}` })}
          </div>
        ))}
      </div>
      {stats.associations === 0 && (
        <p className="border-b border-surface-border bg-muted/40 px-4 py-2 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'wiki.graph.hint.noLinks' })}
        </p>
      )}
      <svg
        ref={svgRef}
        width={width}
        height={height}
        className="w-full text-muted-foreground"
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={intl.formatMessage({ id: 'wiki.graph.aria' }, stats)}
      >
        <title>{intl.formatMessage({ id: 'wiki.tab.graph' })}</title>
        <desc>{intl.formatMessage({ id: 'wiki.graph.desc' })}</desc>
      </svg>
    </div>
  );
}
