import { useEffect, useRef, useCallback } from 'react';
import * as d3 from 'd3';
import type { AgentDetail } from '@/lib/api';

// ── Types ─────────────────────────────────────────────────────

interface OrgNode {
  name: string;
  displayName: string;
  role: string;
  status: string;
  icon: string;
  model: string;
  children?: OrgNode[];
}

interface OrgChartLabels {
  main?: string;
  specialist?: string;
  worker?: string;
  zoom?: string;
}

interface OrgChartProps {
  agents: ReadonlyArray<AgentDetail>;
  onNodeClick?: (agentName: string) => void;
  labels?: OrgChartLabels;
}

// ── Helpers ───────────────────────────────────────────────────

function buildTree(agents: ReadonlyArray<AgentDetail>): OrgNode {
  if (agents.length === 0) {
    return { name: '__root__', displayName: 'DuDuClaw', role: 'system', status: 'active', icon: '\u{1F43E}', model: '', children: [] };
  }

  // Find the root: prefer role=main, then first agent with no reports_to
  const root = agents.find((a) => a.role === 'main')
    ?? agents.find((a) => !a.reports_to || a.reports_to === '');

  const toNode = (agent: AgentDetail, visited = new Set<string>()): OrgNode => {
    // Prevent infinite recursion from circular reports_to
    if (visited.has(agent.name)) {
      return { name: agent.name, displayName: agent.display_name, role: agent.role, status: agent.status, icon: agent.icon || '\u{1F916}', model: agent.model?.preferred ?? '', children: [] };
    }
    const next = new Set(visited);
    next.add(agent.name);
    return {
      name: agent.name,
      displayName: agent.display_name,
      role: agent.role,
      status: agent.status,
      icon: agent.icon || '\u{1F916}',
      model: agent.model?.preferred ?? '',
      children: agents
        .filter((a) => a.reports_to === agent.name && a.name !== agent.name)
        .map((a) => toNode(a, new Set(next))),
    };
  };

  if (root) {
    const rootNode = toNode(root);

    // Find orphans: agents that are NOT the root and whose reports_to
    // is empty OR points to a non-existent agent (and not already a child)
    const childNames = new Set<string>();
    const collectChildNames = (node: OrgNode) => {
      childNames.add(node.name);
      node.children?.forEach(collectChildNames);
    };
    collectChildNames(rootNode);

    const orphans = agents.filter(
      (a) => !childNames.has(a.name) && a.name !== root.name,
    );

    // Only attach top-level orphans (those not reporting to another orphan).
    // toNode() recursively expands children, so attaching all orphans
    // causes agents reporting to a fellow orphan to appear twice.
    if (orphans.length > 0) {
      const orphanNames = new Set(orphans.map((a) => a.name));
      const topOrphans = orphans.filter(
        (a) => !a.reports_to || !orphanNames.has(a.reports_to),
      );
      rootNode.children = [
        ...(rootNode.children ?? []),
        ...topOrphans.map((a) => toNode(a)),
      ];
    }

    return rootNode;
  }

  // No root found — synthetic root grouping all agents
  return {
    name: '__root__',
    displayName: 'DuDuClaw',
    role: 'system',
    status: 'active',
    icon: '\u{1F43E}',
    model: '',
    children: agents.map((a) => toNode(a)),
  };
}

const STATUS_COLORS: Record<string, { fill: string; stroke: string }> = {
  active: { fill: '#ecfdf5', stroke: '#10b981' },
  paused: { fill: '#fffbeb', stroke: '#f59e0b' },
  terminated: { fill: '#fff1f2', stroke: '#f43f5e' },
};

const STATUS_COLORS_DARK: Record<string, { fill: string; stroke: string }> = {
  active: { fill: '#064e3b', stroke: '#34d399' },
  paused: { fill: '#451a03', stroke: '#fbbf24' },
  terminated: { fill: '#4c0519', stroke: '#fb7185' },
};

const ROLE_COLORS: Record<string, string> = {
  main: '#f59e0b',
  specialist: '#3b82f6',
  worker: '#8b5cf6',
  system: '#6b7280',
};

// ── Node dimensions ───────────────────────────────────────────

const NODE_W = 180;
const NODE_H = 64;
const NODE_RX = 10;

// ── Component ─────────────────────────────────────────────────

export function OrgChart({ agents, onNodeClick, labels }: OrgChartProps) {
  const svgRef = useRef<SVGSVGElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  const render = useCallback(() => {
    if (!svgRef.current || !containerRef.current || agents.length === 0)
      return;

    const isDark = document.documentElement.classList.contains('dark');
    const statusColors = isDark ? STATUS_COLORS_DARK : STATUS_COLORS;
    const textColor = isDark ? '#fafaf9' : '#1c1917';
    const subtextColor = isDark ? '#a8a29e' : '#78716c';
    const linkColor = isDark ? '#44403c' : '#d6d3d1';

    const svg = d3.select(svgRef.current);
    svg.selectAll('*').remove();

    const root = d3.hierarchy(buildTree(agents));
    const containerWidth = containerRef.current.clientWidth;
    const containerHeight = containerRef.current.clientHeight;

    // Tree layout — compact spacing
    const treeLayout = d3
      .tree<OrgNode>()
      .nodeSize([NODE_W + 24, NODE_H + 40])
      .separation((a, b) => (a.parent === b.parent ? 1 : 1.1));

    treeLayout(root);

    // Calculate bounds
    let minX = Infinity,
      maxX = -Infinity,
      minY = Infinity,
      maxY = -Infinity;
    root.each((d) => {
      if (d.x! < minX) minX = d.x!;
      if (d.x! > maxX) maxX = d.x!;
      if (d.y! < minY) minY = d.y!;
      if (d.y! > maxY) maxY = d.y!;
    });

    const treeWidth = maxX - minX + NODE_W + 40;
    const treeHeight = maxY - minY + NODE_H + 40;
    const offsetX = -minX + NODE_W / 2 + 20;
    const offsetY = -minY + 20;

    // SVG fills the container — zoom handles the fit
    svg.attr('width', containerWidth).attr('height', containerHeight);

    // Zoom & pan
    const g = svg.append('g');

    const zoom = d3
      .zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.2, 3])
      .on('zoom', (event: d3.D3ZoomEvent<SVGSVGElement, unknown>) => {
        g.attr('transform', event.transform.toString());
      });

    svg.call(zoom);

    // Auto-fit: scale to fit entire tree within the container with padding
    const pad = 32;
    const scaleX = (containerWidth - pad * 2) / treeWidth;
    const scaleY = (containerHeight - pad * 2) / treeHeight;
    const initialScale = Math.min(scaleX, scaleY, 1.2);
    const initialX = (containerWidth - treeWidth * initialScale) / 2;
    const initialY = (containerHeight - treeHeight * initialScale) / 2;

    svg.call(
      zoom.transform,
      d3.zoomIdentity
        .translate(initialX, initialY)
        .scale(initialScale),
    );

    const inner = g.append('g').attr('transform', `translate(${offsetX},${offsetY})`);

    // ── Links (curved) ────────────────────────────────────────
    inner
      .selectAll('path.link')
      .data(root.links())
      .enter()
      .append('path')
      .attr('class', 'link')
      .attr('fill', 'none')
      .attr('stroke', linkColor)
      .attr('stroke-width', 2)
      .attr('d', (d) => {
        const sx = d.source.x!;
        const sy = d.source.y! + NODE_H / 2;
        const tx = d.target.x!;
        const ty = d.target.y! - NODE_H / 2;
        const my = (sy + ty) / 2;
        return `M${sx},${sy} C${sx},${my} ${tx},${my} ${tx},${ty}`;
      });

    // ── Nodes ─────────────────────────────────────────────────
    const nodeGroups = inner
      .selectAll<SVGGElement, d3.HierarchyPointNode<OrgNode>>('g.node')
      .data(root.descendants())
      .enter()
      .append('g')
      .attr('class', 'node')
      .attr(
        'transform',
        (d) => `translate(${d.x! - NODE_W / 2},${d.y! - NODE_H / 2})`,
      )
      .style('cursor', 'pointer')
      .on('click', (_event, d) => {
        if (d.data.name !== '__root__' && onNodeClick) {
          onNodeClick(d.data.name);
        }
      });

    // Card background
    nodeGroups
      .append('rect')
      .attr('width', NODE_W)
      .attr('height', NODE_H)
      .attr('rx', NODE_RX)
      .attr('fill', (d) => statusColors[d.data.status]?.fill ?? '#f5f5f4')
      .attr('stroke', (d) => statusColors[d.data.status]?.stroke ?? '#a8a29e')
      .attr('stroke-width', 2)
      .attr('filter', 'drop-shadow(0 1px 3px rgba(0,0,0,0.1))');

    // Role indicator bar (left edge)
    nodeGroups
      .append('rect')
      .attr('x', 0)
      .attr('y', 0)
      .attr('width', 4)
      .attr('height', NODE_H)
      .attr('rx', 2)
      .attr('fill', (d) => ROLE_COLORS[d.data.role] ?? '#6b7280');

    // Icon
    nodeGroups
      .append('text')
      .attr('x', 16)
      .attr('y', NODE_H / 2 + 1)
      .attr('text-anchor', 'start')
      .attr('dominant-baseline', 'middle')
      .attr('font-size', '20px')
      .text((d) => d.data.icon);

    // Display name
    nodeGroups
      .append('text')
      .attr('x', 44)
      .attr('y', 22)
      .attr('fill', textColor)
      .attr('font-size', '13px')
      .attr('font-weight', '600')
      .text((d) => {
        const name = d.data.displayName;
        return name.length > 14 ? name.slice(0, 13) + '…' : name;
      });

    // Role + status line
    nodeGroups
      .append('text')
      .attr('x', 44)
      .attr('y', 38)
      .attr('fill', subtextColor)
      .attr('font-size', '11px')
      .text((d) => {
        const role = d.data.role.charAt(0).toUpperCase() + d.data.role.slice(1);
        return `${role} · ${d.data.status}`;
      });

    // Model badge
    nodeGroups
      .append('text')
      .attr('x', 44)
      .attr('y', 52)
      .attr('fill', subtextColor)
      .attr('font-size', '10px')
      .attr('opacity', 0.7)
      .text((d) => {
        const m = d.data.model;
        if (!m) return '';
        // Shorten model name: "claude-sonnet-4-6" → "sonnet-4-6"
        return m.replace('claude-', '');
      });

    // Pulse animation for active nodes
    nodeGroups
      .filter((d) => d.data.status === 'active')
      .append('circle')
      .attr('cx', NODE_W - 16)
      .attr('cy', 16)
      .attr('r', 4)
      .attr('fill', statusColors.active.stroke);
  }, [agents, onNodeClick]);

  // Render on mount and when agents change
  useEffect(() => {
    render();

    // Re-render on window resize
    const handleResize = () => render();
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
  }, [render]);

  // Re-render when theme changes
  useEffect(() => {
    const observer = new MutationObserver(() => render());
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class'],
    });
    return () => observer.disconnect();
  }, [render]);

  return (
    <div
      ref={containerRef}
      className="relative h-full min-h-[500px] w-full overflow-hidden rounded-xl border border-stone-200 bg-white dark:border-stone-800 dark:bg-stone-900"
    >
      <svg
        ref={svgRef}
        className="h-full w-full"
        style={{ minHeight: '500px' }}
      />
      {/* Legend — labels passed via props or default (FE-L4) */}
      <div className="absolute bottom-4 left-4 flex gap-4 rounded-lg border border-stone-200 bg-white/90 px-4 py-2 text-xs backdrop-blur-sm dark:border-stone-700 dark:bg-stone-900/90">
        <span className="flex items-center gap-1.5">
          <span
            className="inline-block h-2.5 w-2.5 rounded-full"
            style={{ background: ROLE_COLORS.main }}
          />
          {labels?.main ?? 'Main'}
        </span>
        <span className="flex items-center gap-1.5">
          <span
            className="inline-block h-2.5 w-2.5 rounded-full"
            style={{ background: ROLE_COLORS.specialist }}
          />
          {labels?.specialist ?? 'Specialist'}
        </span>
        <span className="flex items-center gap-1.5">
          <span
            className="inline-block h-2.5 w-2.5 rounded-full"
            style={{ background: ROLE_COLORS.worker }}
          />
          {labels?.worker ?? 'Worker'}
        </span>
      </div>
      {/* Zoom hint */}
      <div className="absolute bottom-4 right-4 rounded-lg border border-stone-200 bg-white/90 px-3 py-1.5 text-xs text-stone-400 backdrop-blur-sm dark:border-stone-700 dark:bg-stone-900/90">
        {labels?.zoom ?? 'Scroll to zoom · Drag to pan'}
      </div>
    </div>
  );
}
