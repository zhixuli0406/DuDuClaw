## Wiki Knowledge Base

This agent maintains a structured wiki at `wiki/`.
Read `wiki/_schema.md` for structure conventions.

### MCP Tools
- `wiki_ls` — List all wiki pages with titles and timestamps
- `wiki_read` — Read a specific page (supports `_index.md`, `_schema.md`)
- `wiki_write` — Create or update a page (auto-updates `_index.md` and `_log.md`)
- `wiki_search` — Full-text search across all pages

### When to update the wiki
- After learning new domain knowledge from conversations
- After receiving user corrections or feedback
- After discovering contradictions with existing knowledge
- When a query answer is worth preserving

### Index-first navigation
Always read `wiki/_index.md` before searching individual pages.
The index contains one-line summaries of every page.

### Knowledge Layers
Pages have a `layer` field controlling injection frequency:
- `identity` (L0) — Agent identity, always in context
- `core` (L1) — Core facts, always in context
- `context` (L2) — Recent decisions, daily refresh
- `deep` (L3) — Deep knowledge, search-only (default)

### Trust Score
Pages have a `trust` field (0.0-1.0) indicating reliability:
- `0.9+` verified, `0.7-0.8` reviewed, `0.5` default, `<0.3` needs review
- Search results are ranked by trust-weighted score

### Page creation workflow
1. Determine the correct subdirectory (`entities/`, `concepts/`, `sources/`, `synthesis/`)
2. Choose a kebab-case filename
3. Write content with YAML frontmatter (title, created, updated, tags, related, sources, layer, trust)
4. Include cross-references to related pages using relative markdown links
5. `wiki_write` handles `_index.md` and `_log.md` updates automatically

### Maintenance
Periodically (suggested: weekly) run a lint pass:
1. Read `_index.md` and check for broken links
2. Identify pages with no inbound links (orphans)
3. Check for contradictions between pages
4. Update stale summaries where newer sources exist
5. Strengthen cross-references between related topics
