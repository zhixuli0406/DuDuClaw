# Wiki Schema

## Directory Structure
- `entities/` — People, organizations, products, customers
- `concepts/` — Domain concepts, processes, principles
- `sources/` — Summaries of raw source materials
- `synthesis/` — Cross-topic analysis, comparisons, trends

## Page Format
Every page MUST have YAML frontmatter:
```yaml
---
title: <page title>
created: <ISO 8601>
updated: <ISO 8601>
tags: [tag1, tag2]
related: [path/to/related1.md, path/to/related2.md]
sources: [source1, source2]
---
```

## Naming Convention
- Filename: kebab-case (e.g. `wang-ming-customer.md`)
- Entity pages: `entities/{name}.md`
- Concept pages: `concepts/{topic}.md`
- Source summaries: `sources/{date}-{title}.md`
- Synthesis: `synthesis/{topic}.md`

## Cross-Reference Format
Use relative markdown links: `[Display Text](../concepts/topic.md)`

## Operations

### Ingest (adding new source)
1. Read the source material
2. Create `sources/{date}-{title}.md` summary
3. Update or create relevant entity/concept pages
4. Update `_index.md` with new pages
5. Check for contradictions with existing pages

### Query (answering questions)
1. Read `_index.md` to locate relevant pages
2. Read relevant pages
3. Synthesize answer
4. If answer is valuable, file as new `synthesis/` page

### Lint (health check)
1. Find contradictions between pages
2. Find orphan pages (not in _index.md or no inbound links)
3. Find stale pages (not updated in >30 days, related sources newer)
4. Suggest missing pages for mentioned-but-uncreated entities
