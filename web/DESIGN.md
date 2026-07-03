# DuDuClaw Dashboard — Design System (Calm Glass)

> Single source of truth for the dashboard refactor. Every page MUST follow this.
> Derived from: existing Liquid Glass brand + `minimalist-ui` / `design-taste-frontend`
> skills + genspark/Linear/Notion clarity references. zh-TW first.

## 1. Design intent (理解設計意圖)

**One line:** A warm, precise companion console that feels calm and uncluttered —
like Linear/genspark, but keeping DuDuClaw's amber-over-graphite "Liquid Glass" soul.

| Pillar | Before (v1) | After (Calm Glass) |
| --- | --- | --- |
| Surface | Glass everywhere (blur on every card) | Glass reserved for **chrome + overlays**; content uses flat **panels** (hairline border, near-opaque) |
| Ambient | Bright orbs + 56px grid + drifting orb | **Quieted** — low-opacity field, finer grid, slower/subtler |
| Color | Amber used liberally | Amber is a **scarce accent** (primary action, active nav, hero metric) |
| Density | 27 flat nav items, dense cards | **6 grouped** nav sections, generous whitespace |
| Rhythm | Ad-hoc per page | Every page = `PageHeader` + `Section`/`Card` grid on a max-width container |

**Anti-goals:** rainbow accents, heavy drop shadows on content, walls of dense controls,
nav that requires scrolling-and-hunting, decorative motion that distracts.

## 2. Tokens

Defined in `src/index.css` (`@theme` + CSS vars). Use utilities, never hardcode hex.

- **Neutral:** `stone-50…950` (cool graphite OKLCH ramp). Text: `stone-900/100`,
  secondary `stone-500/400`, hairline `stone-200/white-8`.
- **Accent:** `amber-500` (primary), `amber-400` (dark-mode active). Use sparingly.
  - **Launcher exception:** the workspace launcher grid (`components/workspace/`)
    is the *only* place a wider colour set is allowed, and *only* on tile icons
    (`launcher-model.ts` `ACCENT_CLASS`, a fixed 8-hue palette). Everything else
    stays amber-over-graphite — no rainbow on controls, text, or borders.
- **Semantic:** success=`emerald`, warning=`amber`, error=`rose`, info=`sky`.
- **Radius:** card `rounded-xl` (0.75rem), control `rounded-lg`, pill `rounded-full` (badges only).
- **Spacing rhythm:** page `px-0` inside `<Page>` (max-w-[1200px] mx-auto), section gap
  `space-y-6`, card padding `p-5`, control gap `gap-3`.
- **Type scale:** page title `text-2xl font-semibold tracking-tight`, section
  `text-sm font-semibold`, body `text-sm`, meta `text-xs text-stone-500`.
  Tabular numerals on data (`tabular-nums`).

## 3. Surfaces

| Utility | Use | Recipe |
| --- | --- | --- |
| `panel` | **Content cards** (default) | near-opaque fill + 1px hairline, no blur, no shadow |
| `panel-hover` | clickable cards | adds subtle lift + border-brighten on hover |
| `glass-chrome` | sidebar / header | heavy blur, no shadow (kept) |
| `glass-overlay` | dialogs / menus / popovers | strong fill + blur (kept) |
| `glass-card` | legacy — **avoid in new code**, migrate to `panel` | |

## 4. Component library (`src/components/ui/`)

Compose pages from these — do NOT re-style raw `<div>`s per page.

- `Page` — max-width container + vertical rhythm wrapper.
- `PageHeader` — title, subtitle/description, optional actions slot, optional icon.
- `Card` — `panel` surface; props: `title`, `actions`, `padded`, `as`, `onClick`.
- `Section` — labeled block (heading + optional description + children).
- `StatCard` — metric tile: label, value, delta, icon, tone.
- `Tabs` — accessible tab strip (keyboard arrow nav) + `TabPanel`.
- `Button` — variants `primary | secondary | ghost | danger`, sizes `sm | md`.
- `Badge` — status pill, tones `neutral|success|warning|danger|info|accent`.
- `EmptyState` — icon + title + hint + optional action.
- `Skeleton` / `SkeletonList` — loading placeholders (prefer over a bare spinner
  for list/table surfaces; `role="status" aria-busy`, reduced-motion safe).
- `Toolbar` — search + filters row above lists.
- `Field` — label + control + help/error (forms).

`Button` also takes `pending` — swaps the leading icon for a spinner, disables,
and sets `aria-busy`; use it on every async submit instead of a hand-rolled state.

Icons: **lucide-react**, 18px (`h-[1.125rem]`) default. (We keep Lucide — the
`minimalist-ui` skill bans it, but our brand is icon-forward; we instead apply its
whitespace/flat/restraint principles.)

## 4b. Shell modes (workspace ⇄ dashboard)

Two top-level shells, switched by `ModeToggle` (Header) and persisted in
`stores/ui-mode-store.ts`:

- **workspace** — Genspark-style consumer surface (`pages/WorkspacePage.tsx`):
  one centred prompt bar (`components/workspace/PromptBar.tsx`, reusing the
  `/ws/chat` pipeline), the Claw value hero, and the launcher grid. The sidebar
  collapses to a narrow icon rail. Default on the `personal` edition.
- **dashboard** — the full Calm Glass console (everything below). Default on
  enterprise / when a preference was already chosen.

The index route (`/`) renders either shell via `App.tsx`'s `HomeRoute`. Role and
edition gating is shared with the launcher via `lib/nav-visibility.ts`.

**Command palette (⌘K / Ctrl+K).** `components/CommandPalette.tsx` (mounted once in
`MainLayout`) is the primary way to move through the 37-page console — the
Raycast-aligned answer to "nav that requires scrolling-and-hunting". It fuzzy-matches
(`lib/fuzzy.ts`, dependency-free, CJK-safe, Latin aliases from each `nav.*` id) across
every role/edition-gated nav route plus quick actions (theme, language, shell mode,
logout). Empty query surfaces recently-visited routes (`stores/command-palette-store.ts`,
persisted MRU). ARIA combobox+listbox, arrow/Enter/Esc keyboard nav. The Header shows a
`Search… ⌘K` trigger for discoverability.

**Mobile shell.** Below `md` the dashboard sidebar is an off-canvas drawer
(`stores/sidebar-store.ts`), toggled by a Header hamburger and dismissed on
navigation or backdrop tap; at `md`+ it is a static column (no behavior change).

## 5. Navigation IA (6 groups)

`src/components/layout/nav-model.ts` is the **single nav source**. Order within group
= frequency. Group headers + collapsible. Role-gating preserved (`minRole`).

1. **總覽 Overview** — Dashboard, WebChat
2. **代理 Agents** — Agents, Tasks, Forks, Org(mgr), Memory
3. **知識 Knowledge** — Knowledge Hub, Shared Wiki, Skills, Marketplace
4. **整合 Integrations** — Channels(adm), MCP(adm), MCP Keys(adm), Odoo(adm), Inference(adm)
5. **營運 Operations** — Accounts(adm), Billing(mgr), Reports(mgr), License(mgr), Partner(mgr)
6. **系統 System** — Security(adm), Governance(adm), Wiki Trust(adm), Reliability(adm), Users(adm), Logs(mgr), Settings(adm)

## 6. Per-page rebuild checklist (可重複方法)

For each page, in order:

1. **Read** the current page; list every data source (stores/api calls) and action.
   Preserve ALL behavior — this is a visual/structure refactor, not a feature change.
2. Wrap in `<Page>` + `<PageHeader title subtitle actions>`.
3. Replace ad-hoc headers/cards with `Card` / `Section` / `StatCard` / `Tabs`.
4. Replace `glass-card` → `panel`; remove per-page drop shadows; normalize spacing to §2.
5. Reduce density: group related controls, move secondary actions into menus/toolbars,
   use `EmptyState` for empty data.
6. Keep i18n: all strings via `intl.formatMessage`; add new keys to all 3 catalogues.
7. **Verify:** `npm run build` (tsc) green; run `audit` + `critique` skill mentally
   (a11y, contrast, overflow, keyboard).
8. Keep diffs behavior-preserving; do not touch store/api signatures.

## 7b. Reusable refactor workflow (把流程變成可重複方法)

The exact, replayable recipe used to ship Calm Glass — re-run it for any future
redesign or when onboarding a new page set:

1. **Understand intent** — capture the target aesthetic + brand constraints in
   this file's §1 table (before/after). Reference design skills: `teach-impeccable`
   (gather context), `critique` (evaluate), `minimalist-ui` / `design-taste-frontend`
   (clean-aesthetic source — adapt, don't blindly apply; we keep Lucide + 🐾 + glass-for-chrome).
2. **Tokens first** — extend `src/index.css` (`@theme` + CSS vars + `@utility`).
   Add surfaces (`panel`/`panel-hover`), calm the ambient. Never hardcode hex in pages.
3. **Primitives** (`src/components/ui/`) — build the small composable set (§4) +
   a barrel `index.ts`. These enforce consistency (point 3); pages compose, never re-style.
4. **Shell + IA** — single nav source (`layout/nav-model.ts`), grouped Sidebar,
   simplified Header. Add group i18n keys to ALL catalogues (zh-TW/en/ja-JP).
5. **Reference page** — migrate ONE page by hand (Dashboard) as the exemplar.
   `npx tsc -b` must be green before fanning out.
6. **Fan out** — migrate remaining pages with parallel sub-agents in batches of ~6.
   Each agent gets: this file + the barrel + the reference page + the §6 checklist.
   **Anti-conflict rules:** agents do NOT edit `src/i18n/` (concurrent-write hazard —
   reuse existing keys, report needed ones) and do NOT run `tsc -b` (build-info race).
   The orchestrator runs ONE consolidated `tsc -b` per batch and fixes fallout.
7. **Verify** (point 4) — `tsc -b` + `npx vitest run` (28 tests) + `npm run build` +
   `audit`/`critique` pass; fix criticals (focus rings, search `aria-label`, contrast).
   **Gotcha:** some `*.test.tsx` assert the page H1 text — keep the page's original
   title i18n key (e.g. `channels.title`, `agents.title`), don't swap to `nav.*`.

## 7. Accessibility & resilience (driven by `audit` / `harden`)

- WCAG 2.1 AA contrast in both themes; focus-visible rings on all interactives.
- Keyboard: tabs arrow-nav, menus Esc-close, dialogs focus-trap.
- Respect `prefers-reduced-motion` / `prefers-reduced-transparency` (already wired).
- Text overflow: truncate + title; tables scroll-x; long CJK wraps.
- Every async surface has loading + empty + error states.
