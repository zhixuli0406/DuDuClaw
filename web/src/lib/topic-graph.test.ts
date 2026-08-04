import { describe, it, expect } from 'vitest';
import {
  buildTopicGraph,
  categoryOf,
  entityLabel,
  pointerToPath,
  extractWikiLinks,
  extractMarkdownLinks,
  extractRelated,
  pageNodeId,
  topicNodeId,
  topicPrimarySource,
} from './topic-graph';

const page = (path: string, title: string, tags: string[] = []) => ({ path, title, tags });

describe('categoryOf', () => {
  it('uses the top-level directory', () => {
    expect(categoryOf('policies/refund.md')).toBe('policies');
  });
  it('skips the auto namespace and uses the doc type', () => {
    expect(categoryOf('auto/charter/team.md')).toBe('charter');
  });
  it('returns empty for root-level pages', () => {
    expect(categoryOf('index.md')).toBe('');
    expect(categoryOf('auto/loose.md')).toBe('');
  });
});

describe('entityLabel', () => {
  it('strips a type prefix', () => {
    expect(entityLabel('person:王明')).toBe('王明');
    expect(entityLabel('product:保險方案')).toBe('保險方案');
  });
  it('leaves plain names and URLs alone', () => {
    expect(entityLabel('保險')).toBe('保險');
    expect(entityLabel('http://example.com')).toBe('http://example.com');
  });
});

describe('pointerToPath', () => {
  it('maps a wiki pointer subject onto a page path', () => {
    expect(pointerToPath('wiki:auto/charter/team')).toBe('auto/charter/team.md');
    expect(pointerToPath('wiki:auto/charter/team.md')).toBe('auto/charter/team.md');
  });
  it('returns empty for non-pointers', () => {
    expect(pointerToPath('person:王明')).toBe('');
  });
});

describe('reference extraction', () => {
  it('reads obsidian wikilinks with aliases and anchors', () => {
    expect(extractWikiLinks('see [[退貨規則]] and [[policies/refund|退款]] and [[FAQ#第一節]]')).toEqual([
      '退貨規則',
      'policies/refund',
      'FAQ',
    ]);
  });
  it('reads local markdown links but not http ones', () => {
    expect(extractMarkdownLinks('[a](policies/refund.md) [b](https://x.com/a.md)')).toEqual([
      'policies/refund.md',
    ]);
  });
  it('reads the related frontmatter list', () => {
    const md = '---\ntitle: A\nrelated: [policies/refund.md, "faq.md"]\n---\nbody';
    expect(extractRelated(md)).toEqual(['policies/refund.md', 'faq.md']);
  });
});

describe('buildTopicGraph — pages + tags', () => {
  it('turns tags and directories into topic nodes with page edges', () => {
    const g = buildTopicGraph({
      pages: [page('policies/refund.md', '退貨規則', ['客服', '政策'])],
    });
    const ids = g.nodes.map((n) => n.id);
    expect(ids).toContain(pageNodeId('policies/refund.md'));
    expect(ids).toContain(topicNodeId('客服'));
    expect(ids).toContain(topicNodeId('政策'));
    expect(ids).toContain(topicNodeId('policies'));
    expect(g.stats).toEqual({ pages: 1, topics: 3, links: 3, associations: 0 });
    expect(g.links.every((l) => l.kind === 'page-topic')).toBe(true);
  });

  it('never leaves a single page as an isolated dot', () => {
    const g = buildTopicGraph(
      { pages: [page('notes.md', '雜記', [])] },
      { uncategorizedLabel: '未分類' },
    );
    expect(g.stats.pages).toBe(1);
    expect(g.stats.links).toBe(1);
    // One page can't be *related* to anything — the UI says so instead of
    // drawing a bare dot.
    expect(g.stats.associations).toBe(0);
    expect(g.nodes.find((n) => n.kind === 'topic')?.label).toBe('未分類');
  });

  it('merges a tag and a directory that normalize to the same topic', () => {
    const g = buildTopicGraph({
      pages: [page('policies/refund.md', '退貨', ['Policies'])],
    });
    const topics = g.nodes.filter((n) => n.kind === 'topic');
    expect(topics).toHaveLength(1);
    expect(topics[0].sources.sort()).toEqual(['category', 'tag']);
    expect(topicPrimarySource(topics[0])).toBe('tag');
    // One merged topic ⇒ one edge, not two parallel ones.
    expect(g.stats.links).toBe(1);
  });
});

describe('buildTopicGraph — shared topics', () => {
  it('links two pages that share a niche topic', () => {
    const g = buildTopicGraph({
      pages: [page('a.md', 'A', ['保險']), page('b.md', 'B', ['保險'])],
    });
    const shared = g.links.filter((l) => l.kind === 'page-page-shared');
    expect(shared).toHaveLength(1);
    expect(shared[0].label).toBe('保險');
    // Shared topic + the pairwise edge both count as real association.
    expect(g.stats.associations).toBeGreaterThan(0);
    expect(new Set([shared[0].source, shared[0].target])).toEqual(
      new Set([pageNodeId('a.md'), pageNodeId('b.md')]),
    );
  });

  it('keeps hub topics hub-only instead of building a hairball', () => {
    const pages = ['a', 'b', 'c', 'd'].map((n) => page(`${n}.md`, n.toUpperCase(), ['共通']));
    const g = buildTopicGraph({ pages }, { maxSharedTopicFanout: 3 });
    expect(g.links.filter((l) => l.kind === 'page-page-shared')).toHaveLength(0);
    expect(g.links.filter((l) => l.kind === 'page-topic')).toHaveLength(4);
  });

  it('accumulates every shared topic onto one edge label', () => {
    const g = buildTopicGraph({
      pages: [page('a.md', 'A', ['保險', '法規']), page('b.md', 'B', ['保險', '法規'])],
    });
    const shared = g.links.filter((l) => l.kind === 'page-page-shared');
    expect(shared).toHaveLength(1);
    expect(shared[0].label?.split('、').sort()).toEqual(['保險', '法規']);
  });
});

describe('buildTopicGraph — memory graph (SPO)', () => {
  const pages = [page('auto/charter/team.md', '團隊章程', []), page('auto/faq/refund.md', '退款 FAQ', [])];

  it('attaches an entity topic to the page its memory pointer belongs to', () => {
    const g = buildTopicGraph({
      pages,
      memoryEdges: [
        // The page's own pointer — a self edge, must be ignored.
        { subject: 'wiki:auto/charter/team', predicate: 'documented_in', object: 'auto/charter/team.md' },
        { subject: 'wiki:auto/charter/team', predicate: 'covers', object: 'person:王明' },
      ],
    });
    const topic = g.nodes.find((n) => n.id === topicNodeId('王明'));
    expect(topic?.sources).toEqual(['entity']);
    const edge = g.links.find(
      (l) => l.source === pageNodeId('auto/charter/team.md') && l.target === topicNodeId('王明'),
    );
    expect(edge?.kind).toBe('page-topic');
    // The documented_in self-pointer produced no edge.
    expect(g.links.some((l) => l.label === 'documented_in')).toBe(false);
  });

  it('links two pages whose pointers are related by a triple', () => {
    const g = buildTopicGraph({
      pages,
      memoryEdges: [
        { subject: 'wiki:auto/charter/team', predicate: 'supersedes', object: 'wiki:auto/faq/refund' },
      ],
    });
    const link = g.links.find((l) => l.kind === 'page-page-link');
    expect(link?.label).toBe('supersedes');
  });

  it('drops entity topics that touch no page', () => {
    const g = buildTopicGraph({
      pages,
      memoryEdges: [{ subject: 'person:張三', predicate: 'works_at', object: 'org:某公司' }],
    });
    expect(g.nodes.some((n) => n.id === topicNodeId('張三'))).toBe(false);
    expect(g.nodes.some((n) => n.id === topicNodeId('某公司'))).toBe(false);
  });

  it('keeps a topic↔topic edge when both topics touch a page', () => {
    const g = buildTopicGraph({
      pages,
      memoryEdges: [
        { subject: 'wiki:auto/charter/team', predicate: 'covers', object: 'person:王明' },
        { subject: 'wiki:auto/faq/refund', predicate: 'covers', object: 'org:某公司' },
        { subject: 'person:王明', predicate: 'works_at', object: 'org:某公司' },
      ],
    });
    const tt = g.links.filter((l) => l.kind === 'topic-topic');
    expect(tt).toHaveLength(1);
    expect(tt[0].label).toBe('works_at');
  });
});

describe('buildTopicGraph — explicit references', () => {
  it('resolves wikilinks by title and markdown links by path', () => {
    const g = buildTopicGraph({
      pages: [page('a.md', '甲頁', ['x']), page('notes/b.md', '乙頁', ['x'])],
      pageContents: {
        'a.md': 'see [[乙頁]]',
      },
    });
    const explicit = g.links.filter((l) => l.kind === 'page-page-link');
    expect(explicit).toHaveLength(1);
    // Explicit reference outranks the weaker shared-topic edge for the same pair.
    expect(g.links.filter((l) => l.kind === 'page-page-shared')).toHaveLength(0);
  });

  it('ignores references to pages that do not exist', () => {
    const g = buildTopicGraph({
      pages: [page('a.md', '甲頁', [])],
      pageContents: { 'a.md': 'see [[不存在的頁]] and [gone](nope.md)' },
    });
    expect(g.links.filter((l) => l.kind === 'page-page-link')).toHaveLength(0);
  });
});

describe('buildTopicGraph — degenerate input', () => {
  it('returns an empty graph for no pages', () => {
    const g = buildTopicGraph({ pages: [] });
    expect(g.nodes).toEqual([]);
    expect(g.links).toEqual([]);
    expect(g.stats).toEqual({ pages: 0, topics: 0, links: 0, associations: 0 });
  });

  it('is deterministic across runs', () => {
    const input = {
      pages: [page('b.md', 'B', ['t']), page('a.md', 'A', ['t'])],
      memoryEdges: [{ subject: 'wiki:a', predicate: 'covers', object: 'person:王明' }],
    };
    expect(JSON.stringify(buildTopicGraph(input))).toBe(JSON.stringify(buildTopicGraph(input)));
  });
});
