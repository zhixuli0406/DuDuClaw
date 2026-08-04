/**
 * topic-graph — assembles the Knowledge Hub 關聯圖 (WP16).
 *
 * The pre-WP16 graph drew page↔page edges only, from markdown links and the
 * `related:` frontmatter list. Knowledge-base pages are written by the agent
 * and almost never cross-link, so a real customer wiki rendered as
 * "1 nodes · 0 links" — a lonely dot that reads as "this feature is broken".
 *
 * WP16 makes the graph **topic-oriented** (the Obsidian mental model the
 * customer expects): topics are first-class nodes, and pages hang off the
 * topics they share.
 *
 * Node kinds
 * - `page`  — one wiki page.
 * - `topic` — a subject the knowledge base talks about. Three provenances,
 *   merged into ONE node when they normalize to the same label:
 *   - `tag`      — frontmatter `tags:` entry.
 *   - `category` — the page's directory (`auto/<doc_type>/x.md` → `<doc_type>`).
 *   - `entity`   — an SPO subject/object from the agent's memory graph
 *                  (`memory.graph`), i.e. something the AI actually learned.
 *
 * Link kinds
 * - `page-topic`       — the page carries that tag / sits in that category /
 *                        its memory pointer participates in that SPO triple.
 * - `page-page-link`   — an explicit reference: `[[wikilink]]`, `](page.md)`,
 *                        `related:` frontmatter, or an SPO triple whose two
 *                        endpoints are both wiki pages.
 * - `page-page-shared` — two pages share a *niche* topic (a topic linking at
 *                        most `maxSharedTopicFanout` pages). Broad topics stay
 *                        hub-only: pairwise edges there would be a hairball.
 * - `topic-topic`      — an SPO triple between two topics that both touch a page.
 *
 * Everything here is pure and synchronous so the assembly rules can be unit
 * tested without d3, the DOM, or a gateway.
 */

// ── Public shapes ───────────────────────────────────────────

export type TopicKind = 'tag' | 'category' | 'entity';
export type NodeKind = 'page' | 'topic';
export type LinkKind = 'page-topic' | 'page-page-link' | 'page-page-shared' | 'topic-topic';

export interface TopicGraphNode {
  /** `page:<path>` or `topic:<normalized label>`. */
  id: string;
  kind: NodeKind;
  /** Human label — page title, or the first-seen spelling of the topic. */
  label: string;
  /** Page path, only for `kind: 'page'`. */
  path?: string;
  /** Where a topic came from; may hold several (a tag that is also a category). */
  sources: TopicKind[];
  /** Incident link count (drives node radius). */
  degree: number;
}

export interface TopicGraphLink {
  source: string;
  target: string;
  kind: LinkKind;
  /** Tooltip detail: shared topic labels, or the SPO predicate. */
  label?: string;
}

export interface TopicGraphPageInput {
  path: string;
  title: string;
  tags: string[];
}

export interface TopicGraphMemoryEdge {
  subject: string;
  predicate: string | null;
  object: string | null;
}

export interface TopicGraphInput {
  pages: ReadonlyArray<TopicGraphPageInput>;
  /** Raw page bodies keyed by path — only used to mine explicit references. */
  pageContents?: Record<string, string>;
  /** SPO triples from `memory.graph`. Optional: the graph works without them. */
  memoryEdges?: ReadonlyArray<TopicGraphMemoryEdge>;
}

export interface TopicGraphOptions {
  /** Label for the bucket that catches pages with no tag and no directory. */
  uncategorizedLabel?: string;
  /**
   * A topic linking more pages than this is a hub: its pages connect to the
   * topic node but get no pairwise `page-page-shared` edges.
   */
  maxSharedTopicFanout?: number;
}

export interface TopicGraphResult {
  nodes: TopicGraphNode[];
  links: TopicGraphLink[];
  stats: {
    pages: number;
    topics: number;
    links: number;
    /**
     * How many *relationships between pages* the graph actually found — topics
     * shared by ≥2 pages, plus page↔page and topic↔topic edges. Every page has
     * at least one page→topic edge by construction, so `links` alone can never
     * answer "is anything related to anything yet?"; this can.
     */
    associations: number;
  };
}

/** Predicate of the auto-filed page's memory pointer — a page→itself edge. */
const WIKI_POINTER_PREDICATE = 'documented_in';
/** Memory pointer subjects are `wiki:<page path without .md>`. */
const WIKI_POINTER_PREFIX = 'wiki:';
/** Auto-filed pages live at `auto/<doc_type>/<slug>.md`; `auto` is not a topic. */
const AUTO_NAMESPACE = 'auto';

const DEFAULT_MAX_SHARED_TOPIC_FANOUT = 6;

// ── Small helpers (exported for tests) ──────────────────────

/**
 * The directory a page belongs to, used as a topic. Root-level pages have
 * none. `auto/<doc_type>/<slug>.md` yields `<doc_type>` — the plain `auto`
 * bucket carries no meaning for the reader.
 */
export function categoryOf(path: string): string {
  const segs = path.split('/').filter(Boolean);
  if (segs.length < 2) return '';
  if (segs[0] === AUTO_NAMESPACE) return segs.length >= 3 ? segs[1] : '';
  return segs[0];
}

/** Case/whitespace-insensitive topic key. CJK is unaffected by `toLowerCase`. */
export function topicKey(label: string): string {
  return label.trim().toLowerCase().replace(/\s+/g, ' ');
}

/**
 * Strip a memory entity's type prefix (`person:王明` → `王明`) so an entity and
 * a same-named tag collapse into one topic. URLs (`http://…`) are left alone.
 */
export function entityLabel(entity: string): string {
  const m = /^[A-Za-z_][A-Za-z0-9_-]*:(.*)$/.exec(entity.trim());
  if (!m) return entity.trim();
  const rest = m[1];
  if (rest.startsWith('//') || rest === '') return entity.trim();
  return rest.trim();
}

/** `wiki:auto/x/y` → `auto/x/y.md`. Returns '' for non-pointer entities. */
export function pointerToPath(entity: string): string {
  if (!entity.startsWith(WIKI_POINTER_PREFIX)) return '';
  const rest = entity.slice(WIKI_POINTER_PREFIX.length).trim();
  if (!rest) return '';
  return rest.endsWith('.md') ? rest : `${rest}.md`;
}

/** Markdown links to local pages: `](notes/foo.md)`. */
export function extractMarkdownLinks(content: string): string[] {
  const out: string[] = [];
  const re = /\]\(([^)\s]+\.md)\)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(content)) !== null) {
    const target = m[1].replace(/^\.\//, '').replace(/^\.\.\//, '');
    if (!/^https?:/i.test(target)) out.push(target);
  }
  return out;
}

/** Obsidian-style `[[Target]]` / `[[Target|alias]]` references. */
export function extractWikiLinks(content: string): string[] {
  const out: string[] = [];
  const re = /\[\[([^\]|#]+)(?:[|#][^\]]*)?\]\]/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(content)) !== null) {
    const t = m[1].trim();
    if (t) out.push(t);
  }
  return out;
}

/** `related: [a.md, b.md]` from YAML frontmatter. */
export function extractRelated(content: string): string[] {
  const trimmed = content.trim();
  if (!trimmed.startsWith('---')) return [];
  const endIdx = trimmed.indexOf('\n---', 3);
  if (endIdx < 0) return [];
  const fm = trimmed.slice(3, endIdx);
  for (const line of fm.split('\n')) {
    const t = line.trim();
    if (!t.startsWith('related:')) continue;
    const inner = t.slice('related:'.length).trim().replace(/^\[/, '').replace(/\]$/, '');
    if (!inner) return [];
    return inner
      .split(',')
      .map((s) => s.trim().replace(/^["']|["']$/g, ''))
      .filter(Boolean);
  }
  return [];
}

// ── Assembly ────────────────────────────────────────────────

/** Higher wins when two rules produce an edge for the same node pair. */
const LINK_PRIORITY: Record<LinkKind, number> = {
  'page-page-link': 3,
  'page-topic': 2,
  'topic-topic': 2,
  'page-page-shared': 1,
};

export function pageNodeId(path: string): string {
  return `page:${path}`;
}

export function topicNodeId(label: string): string {
  return `topic:${topicKey(label)}`;
}

export function buildTopicGraph(
  input: TopicGraphInput,
  options: TopicGraphOptions = {},
): TopicGraphResult {
  const uncategorized = options.uncategorizedLabel ?? '未分類';
  const maxFanout = options.maxSharedTopicFanout ?? DEFAULT_MAX_SHARED_TOPIC_FANOUT;

  const pages = input.pages ?? [];
  const pageContents = input.pageContents ?? {};
  const memoryEdges = input.memoryEdges ?? [];

  const nodes = new Map<string, TopicGraphNode>();
  /** topic node id → page node ids attached to it. */
  const topicPages = new Map<string, Set<string>>();
  /** deduped links, keyed by unordered node pair. */
  const linkByPair = new Map<string, TopicGraphLink>();

  const pagePaths = new Set(pages.map((p) => p.path));

  // Page nodes.
  for (const p of pages) {
    nodes.set(pageNodeId(p.path), {
      id: pageNodeId(p.path),
      kind: 'page',
      label: p.title || p.path,
      path: p.path,
      sources: [],
      degree: 0,
    });
  }

  function ensureTopic(rawLabel: string, source: TopicKind): string | null {
    const label = rawLabel.trim();
    if (!label) return null;
    const id = topicNodeId(label);
    const existing = nodes.get(id);
    if (existing) {
      if (existing.kind === 'topic' && !existing.sources.includes(source)) {
        existing.sources = [...existing.sources, source];
      }
      return existing.kind === 'topic' ? id : null;
    }
    nodes.set(id, { id, kind: 'topic', label, sources: [source], degree: 0 });
    return id;
  }

  function pairKey(a: string, b: string): string {
    return a < b ? `${a} ${b}` : `${b} ${a}`;
  }

  function addLink(a: string, b: string, kind: LinkKind, label?: string): void {
    if (a === b) return;
    if (!nodes.has(a) || !nodes.has(b)) return;
    const key = pairKey(a, b);
    const prev = linkByPair.get(key);
    if (prev && LINK_PRIORITY[prev.kind] >= LINK_PRIORITY[kind]) return;
    linkByPair.set(key, { source: a, target: b, kind, label });
  }

  function attachTopic(pageId: string, topicId: string): void {
    addLink(pageId, topicId, 'page-topic');
    const set = topicPages.get(topicId) ?? new Set<string>();
    set.add(pageId);
    topicPages.set(topicId, set);
  }

  // ① page → topic (tags + category). A page with neither lands in the
  //    uncategorized bucket rather than floating as a misleading lone dot.
  for (const p of pages) {
    const pid = pageNodeId(p.path);
    let attached = 0;
    for (const tag of p.tags ?? []) {
      const tid = ensureTopic(tag, 'tag');
      if (tid) {
        attachTopic(pid, tid);
        attached += 1;
      }
    }
    const cat = categoryOf(p.path);
    if (cat) {
      const cid = ensureTopic(cat, 'category');
      if (cid) {
        attachTopic(pid, cid);
        attached += 1;
      }
    }
    if (attached === 0) {
      const uid = ensureTopic(uncategorized, 'category');
      if (uid) attachTopic(pid, uid);
    }
  }

  // ② memory graph (SPO) → topics. An endpoint is a page when it is a wiki
  //    pointer subject (`wiki:<path>`) or a literal page path; otherwise it is
  //    an entity topic. Entity topics that never touch a page are dropped so
  //    the whole memory graph doesn't flood the wiki view.
  type Endpoint = { kind: 'page'; id: string } | { kind: 'entity'; label: string } | null;
  const resolveEndpoint = (raw: string | null | undefined): Endpoint => {
    const v = (raw ?? '').trim();
    if (!v) return null;
    const asPointer = pointerToPath(v);
    if (asPointer) return pagePaths.has(asPointer) ? { kind: 'page', id: pageNodeId(asPointer) } : null;
    if (pagePaths.has(v)) return { kind: 'page', id: pageNodeId(v) };
    // A dangling `.md` reference is a page we don't have — not a topic.
    if (v.endsWith('.md')) return null;
    return { kind: 'entity', label: entityLabel(v) };
  };

  const entityEntityEdges: Array<{ a: string; b: string; label: string }> = [];

  for (const e of memoryEdges) {
    const src = resolveEndpoint(e.subject);
    const dst = resolveEndpoint(e.object);
    if (!src || !dst) continue;
    // The auto-filed page's own pointer (`wiki:x --documented_in--> x.md`) is
    // a self-edge; it carries no association.
    if (e.predicate === WIKI_POINTER_PREDICATE && src.kind === 'page' && dst.kind === 'page') {
      continue;
    }
    const predicate = (e.predicate ?? '').trim();
    if (src.kind === 'page' && dst.kind === 'page') {
      addLink(src.id, dst.id, 'page-page-link', predicate || undefined);
    } else if (src.kind === 'page' && dst.kind === 'entity') {
      const tid = ensureTopic(dst.label, 'entity');
      if (tid) attachTopic(src.id, tid);
    } else if (src.kind === 'entity' && dst.kind === 'page') {
      const tid = ensureTopic(src.label, 'entity');
      if (tid) attachTopic(dst.id, tid);
    } else if (src.kind === 'entity' && dst.kind === 'entity') {
      entityEntityEdges.push({ a: src.label, b: dst.label, label: predicate });
    }
  }

  // ③ topic ↔ topic, but only between topics that already touch a page.
  for (const { a, b, label } of entityEntityEdges) {
    const aid = topicNodeId(a);
    const bid = topicNodeId(b);
    if (!topicPages.has(aid) || !topicPages.has(bid)) continue;
    addLink(aid, bid, 'topic-topic', label || undefined);
  }

  // ④ explicit page→page references (wikilinks / markdown links / `related:`).
  const byNormTitle = new Map<string, string>();
  const byBasename = new Map<string, string>();
  for (const p of pages) {
    const t = topicKey(p.title || '');
    if (t && !byNormTitle.has(t)) byNormTitle.set(t, p.path);
    const base = topicKey(p.path.split('/').pop()?.replace(/\.md$/, '') ?? '');
    if (base && !byBasename.has(base)) byBasename.set(base, p.path);
  }
  const resolveRef = (ref: string): string | null => {
    const raw = ref.trim();
    if (!raw) return null;
    if (pagePaths.has(raw)) return raw;
    const withMd = raw.endsWith('.md') ? raw : `${raw}.md`;
    if (pagePaths.has(withMd)) return withMd;
    const key = topicKey(raw.replace(/\.md$/, ''));
    return byNormTitle.get(key) ?? byBasename.get(key) ?? null;
  };

  for (const p of pages) {
    const content = pageContents[p.path] ?? '';
    if (!content) continue;
    const refs = new Set([
      ...extractRelated(content),
      ...extractMarkdownLinks(content),
      ...extractWikiLinks(content),
    ]);
    for (const ref of refs) {
      const target = resolveRef(ref);
      if (target && target !== p.path) {
        addLink(pageNodeId(p.path), pageNodeId(target), 'page-page-link');
      }
    }
  }

  // ⑤ page ↔ page through a *niche* shared topic. Hub topics (many pages) are
  //    left as hubs — pairwise edges there produce an unreadable hairball.
  const sharedLabels = new Map<string, string[]>();
  for (const [tid, pageIds] of topicPages) {
    if (pageIds.size < 2 || pageIds.size > maxFanout) continue;
    const label = nodes.get(tid)?.label ?? '';
    const list = [...pageIds].sort();
    for (let i = 0; i < list.length; i += 1) {
      for (let j = i + 1; j < list.length; j += 1) {
        const key = pairKey(list[i], list[j]);
        const acc = sharedLabels.get(key) ?? [];
        if (label && !acc.includes(label)) acc.push(label);
        sharedLabels.set(key, acc);
        addLink(list[i], list[j], 'page-page-shared', acc.join('、'));
      }
    }
  }
  // Re-stamp the accumulated label list (later topics widen an earlier edge).
  for (const [key, labels] of sharedLabels) {
    const link = linkByPair.get(key);
    if (link && link.kind === 'page-page-shared' && labels.length > 0) {
      link.label = labels.join('、');
    }
  }

  const links = [...linkByPair.values()];

  // Degrees.
  for (const l of links) {
    const s = nodes.get(l.source);
    const t = nodes.get(l.target);
    if (s) s.degree += 1;
    if (t) t.degree += 1;
  }

  // Deterministic order: pages by path, then topics by label.
  const nodeList = [...nodes.values()].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'page' ? -1 : 1;
    return (a.path ?? a.label).localeCompare(b.path ?? b.label);
  });
  const linkList = links.sort((a, b) =>
    a.source === b.source ? a.target.localeCompare(b.target) : a.source.localeCompare(b.source),
  );

  const sharedTopics = [...topicPages.values()].filter((s) => s.size >= 2).length;
  const crossLinks = linkList.filter((l) => l.kind !== 'page-topic').length;

  return {
    nodes: nodeList,
    links: linkList,
    stats: {
      pages: nodeList.filter((n) => n.kind === 'page').length,
      topics: nodeList.filter((n) => n.kind === 'topic').length,
      links: linkList.length,
      associations: sharedTopics + crossLinks,
    },
  };
}

/** Colour a topic node by its strongest provenance (tag > category > entity). */
export function topicPrimarySource(node: TopicGraphNode): TopicKind {
  if (node.sources.includes('tag')) return 'tag';
  if (node.sources.includes('category')) return 'category';
  return 'entity';
}
