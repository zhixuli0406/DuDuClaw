import { useRef, useEffect, useMemo } from 'react';
import * as d3 from 'd3';
import type { WikiPageMeta } from '@/lib/api';

interface GraphNode extends d3.SimulationNodeDatum {
  id: string;
  title: string;
  dir: string;
  tags: string[];
}

interface GraphLink extends d3.SimulationLinkDatum<GraphNode> {
  source: string;
  target: string;
}

interface WikiGraphProps {
  pages: ReadonlyArray<WikiPageMeta>;
  /** Raw page contents keyed by path, used to extract cross-references. */
  pageContents: Record<string, string>;
  width?: number;
  height?: number;
  onSelectPage?: (path: string) => void;
}

/** Directory → color mapping following the DuDuClaw warm palette. */
const DIR_COLORS: Record<string, string> = {
  entities: '#f59e0b',   // amber-500
  concepts: '#3b82f6',   // blue-500
  sources: '#10b981',    // emerald-500
  synthesis: '#8b5cf6',  // violet-500
};
const DEFAULT_COLOR = '#78716c'; // stone-500

function dirColor(path: string): string {
  const dir = path.split('/')[0] ?? '';
  return DIR_COLORS[dir] ?? DEFAULT_COLOR;
}

/** Extract markdown links from page content: `[text](target.md)` */
function extractLinks(content: string): string[] {
  const links: string[] = [];
  // Match ](path.md) — use a more permissive pattern that handles paths with
  // special chars but stops at the .md) boundary
  const re = /\]\(([^)\s]+\.md)\)/g;
  let match;
  while ((match = re.exec(content)) !== null) {
    const target = match[1].replace(/^\.\.\//, '');
    if (!target.startsWith('http')) {
      links.push(target);
    }
  }
  return links;
}

/** Extract `related: [...]` from YAML frontmatter. */
function extractRelated(content: string): string[] {
  const trimmed = content.trim();
  if (!trimmed.startsWith('---')) return [];
  const endIdx = trimmed.indexOf('\n---', 3);
  if (endIdx < 0) return [];
  const fm = trimmed.slice(3, endIdx);
  for (const line of fm.split('\n')) {
    const t = line.trim();
    if (t.startsWith('related:')) {
      const val = t.slice(8).trim();
      const inner = val.replace(/^\[/, '').replace(/\]$/, '');
      if (!inner) return [];
      return inner.split(',').map((s) => s.trim().replace(/^["']|["']$/g, '')).filter(Boolean);
    }
  }
  return [];
}

export function WikiGraph({ pages, pageContents, width = 700, height = 500, onSelectPage }: WikiGraphProps) {
  const svgRef = useRef<SVGSVGElement>(null);
  // Stable ref for callback to avoid effect re-runs
  const onSelectPageRef = useRef(onSelectPage);
  useEffect(() => { onSelectPageRef.current = onSelectPage; }, [onSelectPage]);

  const { nodes, links } = useMemo(() => {
    const pathSet = new Set(pages.map((p) => p.path));
    const nodeMap = new Map<string, GraphNode>();

    for (const page of pages) {
      nodeMap.set(page.path, {
        id: page.path,
        title: page.title,
        dir: page.path.split('/')[0] ?? '',
        tags: page.tags,
      });
    }

    const linkSet = new Set<string>();
    const graphLinks: GraphLink[] = [];

    for (const page of pages) {
      const content = pageContents[page.path] ?? '';
      const targets = [...new Set([...extractRelated(content), ...extractLinks(content)])];
      for (const target of targets) {
        if (pathSet.has(target) && target !== page.path) {
          const key = [page.path, target].sort().join('→');
          if (!linkSet.has(key)) {
            linkSet.add(key);
            graphLinks.push({ source: page.path, target });
          }
        }
      }
    }

    return { nodes: Array.from(nodeMap.values()), links: graphLinks };
  }, [pages, pageContents]);

  useEffect(() => {
    if (!svgRef.current || nodes.length === 0) return;

    const svg = d3.select(svgRef.current);
    svg.selectAll('*').remove();

    const g = svg.append('g');

    // Zoom
    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.3, 4])
      .on('zoom', (event) => g.attr('transform', event.transform));
    svg.call(zoom);

    // Inbound link count for node sizing
    const inbound = new Map<string, number>();
    for (const link of links) {
      const t = typeof link.target === 'string' ? link.target : (link.target as GraphNode).id;
      inbound.set(t, (inbound.get(t) ?? 0) + 1);
    }

    const simulation = d3.forceSimulation<GraphNode>(nodes)
      .force('link', d3.forceLink<GraphNode, GraphLink>(links).id((d) => d.id).distance(80))
      .force('charge', d3.forceManyBody().strength(-200))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .force('collision', d3.forceCollide().radius(25));

    // Links
    const link = g.append('g')
      .selectAll('line')
      .data(links)
      .join('line')
      .attr('stroke', '#a8a29e')
      .attr('stroke-opacity', 0.4)
      .attr('stroke-width', 1);

    // Nodes
    const node = g.append('g')
      .selectAll<SVGCircleElement, GraphNode>('circle')
      .data(nodes)
      .join('circle')
      .attr('r', (d) => 5 + (inbound.get(d.id) ?? 0) * 2)
      .attr('fill', (d) => dirColor(d.id))
      .attr('stroke', '#fafaf9')
      .attr('stroke-width', 1.5)
      .attr('cursor', 'pointer')
      .on('click', (_event, d) => onSelectPageRef.current?.(d.id))
      .call(
        d3.drag<SVGCircleElement, GraphNode>()
          .on('start', (event, d) => {
            if (!event.active) simulation.alphaTarget(0.3).restart();
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
          })
      );

    // Labels
    const label = g.append('g')
      .selectAll('text')
      .data(nodes)
      .join('text')
      .text((d) => d.title.length > 20 ? d.title.slice(0, 18) + '...' : d.title)
      .attr('font-size', 10)
      .attr('dx', 10)
      .attr('dy', 3)
      .attr('fill', '#78716c')
      .attr('pointer-events', 'none');

    // Tooltips
    node.append('title').text((d) => `${d.title}\n${d.id}`);

    simulation.on('tick', () => {
      link
        .attr('x1', (d) => (d.source as unknown as GraphNode).x ?? 0)
        .attr('y1', (d) => (d.source as unknown as GraphNode).y ?? 0)
        .attr('x2', (d) => (d.target as unknown as GraphNode).x ?? 0)
        .attr('y2', (d) => (d.target as unknown as GraphNode).y ?? 0);

      node
        .attr('cx', (d) => d.x ?? 0)
        .attr('cy', (d) => d.y ?? 0);

      label
        .attr('x', (d) => d.x ?? 0)
        .attr('y', (d) => d.y ?? 0);
    });

    return () => {
      simulation.stop();
      svg.on('.zoom', null); // Remove zoom event listeners
      svg.selectAll('*').remove(); // Clear all child elements
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps — onSelectPage ref is stable
  }, [nodes, links, width, height]);

  if (nodes.length === 0) {
    return (
      <div className="flex items-center justify-center rounded-xl border border-dashed border-stone-300 bg-white py-12 dark:border-stone-700 dark:bg-stone-900">
        <p className="text-stone-400">No pages to visualize</p>
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-stone-200 bg-white dark:border-stone-800 dark:bg-stone-900 overflow-hidden">
      {/* Legend */}
      <div className="flex items-center gap-4 px-4 py-2 border-b border-stone-200 dark:border-stone-800">
        {Object.entries(DIR_COLORS).map(([dir, color]) => (
          <div key={dir} className="flex items-center gap-1.5 text-xs text-stone-500">
            <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ backgroundColor: color }} />
            {dir}
          </div>
        ))}
      </div>
      <svg
        ref={svgRef}
        width={width}
        height={height}
        className="w-full"
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label={`Wiki knowledge graph with ${nodes.length} pages and ${links.length} connections`}
      >
        <title>Wiki Knowledge Graph</title>
        <desc>Interactive force-directed graph showing wiki page relationships. Drag nodes to reposition. Click nodes to open pages. Scroll to zoom.</desc>
      </svg>
    </div>
  );
}
