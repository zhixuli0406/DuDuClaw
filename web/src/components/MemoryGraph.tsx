import { useRef, useEffect, useMemo } from 'react';
import * as d3 from 'd3';
import type { MemoryGraphNode, MemoryGraphEdge } from '@/lib/api';

/**
 * MemoryGraph — force-directed viewer for an agent's SPO knowledge graph
 * (design D6). Nodes are entities (radius ∝ degree); links are predicate-labelled
 * SPO edges. Colour encodes source trust tier: high (≥0.7) green, medium
 * (0.3–0.7) amber, low or held-for-review red. Clicking a link surfaces the
 * fact's provenance in the caller's side panel via `onSelectEdge`.
 *
 * Palette is intentionally NOT tokenised (web/DESIGN.md §5.1 — the d3 graph
 * carries its own node/edge colours, like WikiGraph).
 */

interface GNode extends d3.SimulationNodeDatum {
  id: string;
  degree: number;
  /** Colour tier derived from the worst incident edge. */
  tier: Tier;
}

interface GLink extends d3.SimulationLinkDatum<GNode> {
  source: string | GNode;
  target: string | GNode;
  edge: MemoryGraphEdge;
  tier: Tier;
}

type Tier = 'high' | 'medium' | 'low';

const TIER_COLOR: Record<Tier, string> = {
  high: '#10b981', // emerald-500 — trusted source
  medium: '#f59e0b', // amber-500 — ordinary source
  low: '#ef4444', // red-500 — low trust / quarantined
};

function edgeTier(e: MemoryGraphEdge): Tier {
  if (e.quarantined) return 'low';
  if (e.origin_trust >= 0.7) return 'high';
  if (e.origin_trust >= 0.3) return 'medium';
  return 'low';
}

const TIER_RANK: Record<Tier, number> = { high: 0, medium: 1, low: 2 };

interface MemoryGraphProps {
  nodes: ReadonlyArray<MemoryGraphNode>;
  edges: ReadonlyArray<MemoryGraphEdge>;
  width?: number;
  height?: number;
  onSelectEdge?: (edge: MemoryGraphEdge) => void;
  /** id of the memory currently shown in the side panel (highlighted). */
  selectedMemoryId?: string | null;
}

export function MemoryGraph({
  nodes: rawNodes,
  edges: rawEdges,
  width = 900,
  height = 550,
  onSelectEdge,
  selectedMemoryId,
}: MemoryGraphProps) {
  const svgRef = useRef<SVGSVGElement>(null);
  const onSelectEdgeRef = useRef(onSelectEdge);
  useEffect(() => { onSelectEdgeRef.current = onSelectEdge; }, [onSelectEdge]);

  const { nodes, links } = useMemo(() => {
    // Worst-tier per entity (a node touched by any low-trust edge reads red).
    const worstTier = new Map<string, Tier>();
    const bump = (entity: string, tier: Tier) => {
      const cur = worstTier.get(entity);
      if (cur === undefined || TIER_RANK[tier] > TIER_RANK[cur]) worstTier.set(entity, tier);
    };
    const links: GLink[] = [];
    for (const e of rawEdges) {
      if (!e.object) continue; // only subject→object edges are drawable
      const tier = edgeTier(e);
      bump(e.subject, tier);
      bump(e.object, tier);
      links.push({ source: e.subject, target: e.object, edge: e, tier });
    }
    const nodes: GNode[] = rawNodes.map((n) => ({
      id: n.entity,
      degree: n.degree,
      tier: worstTier.get(n.entity) ?? 'high',
    }));
    // Drop links whose endpoints aren't in the node set (defensive).
    const nodeIds = new Set(nodes.map((n) => n.id));
    const validLinks = links.filter((l) => nodeIds.has(l.source as string) && nodeIds.has(l.target as string));
    return { nodes, links: validLinks };
  }, [rawNodes, rawEdges]);

  useEffect(() => {
    if (!svgRef.current || nodes.length === 0) return;

    const reduceMotion =
      typeof window !== 'undefined' &&
      window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;

    const svg = d3.select(svgRef.current);
    svg.selectAll('*').remove();
    const g = svg.append('g');

    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.3, 4])
      .on('zoom', (event) => g.attr('transform', event.transform));
    svg.call(zoom);

    // Work on a copy — d3 mutates x/y/vx/vy in place.
    const simNodes: GNode[] = nodes.map((n) => ({ ...n }));
    const simLinks: GLink[] = links.map((l) => ({ ...l }));

    const simulation = d3.forceSimulation<GNode>(simNodes)
      .force('link', d3.forceLink<GNode, GLink>(simLinks).id((d) => d.id).distance(90))
      .force('charge', d3.forceManyBody().strength(-220))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collision', d3.forceCollide().radius(26));

    const link = g.append('g')
      .selectAll('line')
      .data(simLinks)
      .join('line')
      .attr('stroke', (d) => TIER_COLOR[d.tier])
      .attr('stroke-opacity', (d) => (d.edge.memory_id === selectedMemoryId ? 0.95 : 0.45))
      .attr('stroke-width', (d) => (d.edge.memory_id === selectedMemoryId ? 3 : 1.5))
      .attr('stroke-dasharray', (d) => (d.edge.quarantined ? '4 3' : null))
      .attr('cursor', 'pointer')
      .on('click', (_event, d) => onSelectEdgeRef.current?.(d.edge));
    link.append('title').text((d) => {
      const p = d.edge.predicate ?? '';
      return `${d.edge.subject} —${p}→ ${d.edge.object}`;
    });

    const nodeRadius = (d: GNode) => 5 + Math.min(d.degree, 10) * 1.8;

    const node = g.append('g')
      .selectAll<SVGCircleElement, GNode>('circle')
      .data(simNodes)
      .join('circle')
      .attr('r', nodeRadius)
      .attr('fill', (d) => TIER_COLOR[d.tier])
      .attr('stroke', '#ffffff')
      .attr('stroke-width', 1.5)
      .attr('cursor', 'grab')
      .call(
        d3.drag<SVGCircleElement, GNode>()
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
          }),
      );
    node.append('title').text((d) => `${d.id} · ${d.degree}`);

    const label = g.append('g')
      .selectAll('text')
      .data(simNodes)
      .join('text')
      .text((d) => (d.id.length > 18 ? d.id.slice(0, 16) + '…' : d.id))
      .attr('font-size', 10)
      .attr('dx', 11)
      .attr('dy', 3)
      .attr('fill', 'currentColor')
      .attr('opacity', 0.7)
      .attr('pointer-events', 'none');

    const tick = () => {
      const nx = (n: string | GNode) => (typeof n === 'object' ? n.x ?? 0 : 0);
      const ny = (n: string | GNode) => (typeof n === 'object' ? n.y ?? 0 : 0);
      link
        .attr('x1', (d) => nx(d.source))
        .attr('y1', (d) => ny(d.source))
        .attr('x2', (d) => nx(d.target))
        .attr('y2', (d) => ny(d.target));
      node.attr('cx', (d) => d.x ?? 0).attr('cy', (d) => d.y ?? 0);
      label.attr('x', (d) => d.x ?? 0).attr('y', (d) => d.y ?? 0);
    };

    if (reduceMotion) {
      // No animation: settle synchronously, then render one static frame.
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
  }, [nodes, links, width, height, selectedMemoryId]);

  return (
    <svg
      ref={svgRef}
      width={width}
      height={height}
      className="w-full text-muted-foreground"
      viewBox={`0 0 ${width} ${height}`}
      role="img"
      aria-label={`知識圖譜：${nodes.length} 個實體、${links.length} 條關係`}
    >
      <title>知識圖譜</title>
      <desc>力導向知識圖：拖曳節點可重新排列、捲動可縮放、點擊關係可查看來源。顏色代表知識來源可信度。</desc>
    </svg>
  );
}
