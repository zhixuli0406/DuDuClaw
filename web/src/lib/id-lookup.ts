/**
 * ID-shaped query detection + cross-object exact-id matching for the command
 * palette's "paste an object id, jump straight to it" flow (W3-3, Stripe
 * pattern B4 — "search terms are included in the URL, you can bookmark or
 * share"; here the analogue is "paste an object id into search, land on it").
 *
 * Pure and side-effect free — the palette component owns the actual RPC
 * fan-out (it already has to, to cover tasks/approvals/installs that may not
 * be loaded into any store yet); this module only decides "does this look
 * like a pasted id" and "which of these already-fetched lists does it match".
 */

/** CJK block check — a query containing CJK is a name/keyword search, never
 *  a pasted id (every id format in this app is ASCII: UUID v4, colon/hash
 *  delimited session ids, kebab-case agent slugs). */
const CJK_RE = /[぀-ヿ㐀-䶿一-鿿가-힯豈-﫿]/;

/**
 * Heuristic: does `raw` look like a pasted object id rather than a natural-
 * language search phrase? Task/approval/install ids are UUID v4
 * (`xxxxxxxx-xxxx-...`); conversation ids are colon/hash-delimited
 * (`webchat:webchat:<tag>:<conn>#agent:<id>#conv:<nonce>`, `telegram:12345`);
 * agent ids are short kebab-case slugs. All four share one shape: no
 * whitespace, no CJK, alphanumeric plus `-_:#`.
 *
 * A plain short English word (e.g. "settings") also matches this shape —
 * that is fine. The palette only spends the RPC round-trip on this path when
 * the ordinary nav/entity search already came back empty (see
 * `CommandPalette`), so a false positive here costs one background fetch,
 * never a wrong navigation.
 */
export function looksLikeIdQuery(raw: string): boolean {
  const q = raw.trim();
  if (q.length < 6) return false;
  if (/\s/.test(q)) return false;
  if (CJK_RE.test(q)) return false;
  return /^[A-Za-z0-9][A-Za-z0-9:_#-]*[A-Za-z0-9]$/.test(q);
}

export type IdMatchKind = 'task' | 'approval' | 'install' | 'agent' | 'conversation';

export interface IdMatch {
  readonly kind: IdMatchKind;
  readonly id: string;
}

export interface IdMatchSources {
  readonly tasks?: ReadonlyArray<{ id: string }>;
  readonly approvals?: ReadonlyArray<{ id: string }>;
  readonly installs?: ReadonlyArray<{ id: string }>;
  readonly agents?: ReadonlyArray<{ name: string }>;
  readonly conversations?: ReadonlyArray<{ session_id: string }>;
}

/**
 * Exact-id match across every object kind the palette can jump to. Checked in
 * a fixed, deterministic order (task → approval → install → agent →
 * conversation) so a colliding id across kinds — extremely unlikely for
 * UUIDs, but not impossible for the shorter agent/conversation forms —
 * resolves the same way every time.
 */
export function findIdMatch(query: string, sources: IdMatchSources): IdMatch | null {
  const q = query.trim();
  if (!q) return null;

  const task = sources.tasks?.find((t) => t.id === q);
  if (task) return { kind: 'task', id: task.id };

  const approval = sources.approvals?.find((a) => a.id === q);
  if (approval) return { kind: 'approval', id: approval.id };

  const install = sources.installs?.find((i) => i.id === q);
  if (install) return { kind: 'install', id: install.id };

  const agent = sources.agents?.find((a) => a.name === q);
  if (agent) return { kind: 'agent', id: agent.name };

  const conversation = sources.conversations?.find((c) => c.session_id === q);
  if (conversation) return { kind: 'conversation', id: conversation.session_id };

  return null;
}

/**
 * Route destination for a resolved match. Returns `null` for `conversation`
 * — resuming a conversation is not a plain navigation (it must replay the
 * session into the chat store first), so the caller handles that kind
 * specially instead of routing through this table.
 */
export function idMatchRoute(match: IdMatch): string | null {
  switch (match.kind) {
    case 'task':
      return `/tasks/${encodeURIComponent(match.id)}`;
    case 'approval':
    case 'install':
      // InboxPage's deep-link resolver matches a bare id via suffix fallback
      // (`item.id.endsWith(':' + id)`), so the bare id works for both kinds.
      return `/inbox?item=${encodeURIComponent(match.id)}`;
    case 'agent':
      return `/agents/${encodeURIComponent(match.id)}`;
    case 'conversation':
      return null;
  }
}
