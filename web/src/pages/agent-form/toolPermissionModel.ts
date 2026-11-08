import type { ActionRule, BuiltinToolEntry } from '@/lib/api';

/**
 * RM-026 — pure mapping between the 「工具權限」 table and the three
 * `[capabilities]` lists the gateway actually enforces.
 *
 * ── Mapping (read this before changing anything) ─────────────────────────────
 * 可使用  = NOT matched by `denied_tools`, AND (`allowed_tools` is empty OR matched
 *           by `allowed_tools`). This is `duduclaw_core::tool_catalog::
 *           tool_list_verdict` (denied always wins; non-empty allowlist = allowlist
 *           mode). `entryMatches` below mirrors `tool_entry_matches` (anchored,
 *           server-aware, `*` only as a trailing wildcard).
 *
 * 需審批  = `approval_required_tools` (bare tool names). That is the list the MCP
 *           dispatch gate enforces for EVERY MCP tool (`mcp/approval.rs::
 *           static_gate_flags` → `approval::tool_requires_approval`), and it also
 *           works for the computer-use route. `action_rules {tool, verdict:"ask"}`
 *           is folded into the same gate (take-the-stricter), so a tool that is
 *           asked by a rule is SHOWN as 需審批 but locked: unticking it would need an
 *           action-rule edit, which belongs to 進階設定. We write the plain list
 *           because it is the oldest, simplest, bare-name form and the one the
 *           existing 審批清單 editor already shows — the two stay in sync.
 *
 * ── Writing back — never rewrite what the operator wrote by hand ────────────
 *  • Ticking 可使用 ON:  remove the EXACT (non-wildcard) `denied_tools` entries that
 *    name the tool. If a wildcard deny (`mcp__duduclaw__*`, `*`, `mcp__duduclaw`,
 *    `memory_*`) covers it, the row is locked (it cannot be allowed without
 *    editing/expanding that wildcard) — the table says so, 進階設定 edits it.
 *    In allowlist mode, a tool the allowlist does not cover is ADDED to
 *    `allowed_tools` by its qualified name.
 *  • Ticking 可使用 OFF: ALWAYS append the tool's qualified name to `denied_tools`
 *    (denied wins). `allowed_tools` is never shrunk: removing the last entry would
 *    flip the agent out of allowlist mode and silently allow EVERYTHING, and
 *    removing from a wildcard allowlist would mean expanding it. So an allowlist
 *    such as `mcp__duduclaw__*` stays byte-for-byte as written, the table shows
 *    every covered tool as allowed, and unticking one tool just adds it to the
 *    denylist. Re-ticking removes that denylist entry again → the round trip is
 *    exact.
 *  • 需審批 ON: append the BARE tool name to `approval_required_tools`. OFF: remove
 *    the exact entries naming it; a wildcard approval entry (`shared_wiki_*`) or an
 *    `ask` rule locks the row instead of being expanded/dropped.
 *  • Wildcards in any list are never expanded, split or dropped by this module.
 *  • Only rows the operator changed are touched; every other entry, including
 *    entries for tools that are not in the catalog (custom MCP servers), passes
 *    through unchanged and in the same order.
 */

export interface ToolLists {
  allowed: ReadonlyArray<string>;
  denied: ReadonlyArray<string>;
  approval: ReadonlyArray<string>;
  actionRules?: ReadonlyArray<ActionRule>;
}

export interface Desired {
  allowed: boolean;
  approval: boolean;
}

export interface RowState extends Desired {
  /** Cannot be turned ON: a wildcard deny covers it. */
  allowLocked: boolean;
  /** Cannot be turned OFF: a wildcard approval entry or an `ask` rule covers it. */
  approvalLocked: boolean;
}

// ── Matcher (mirror of duduclaw_core::tool_catalog::tool_entry_matches) ──────

const DUDUCLAW_SERVER = 'duduclaw';

function splitToolRef(raw: string): [string, string] {
  const e = (raw.split('(')[0] ?? raw).trim();
  if (e.startsWith('mcp__')) {
    const rest = e.slice(5);
    const idx = rest.indexOf('__');
    return idx >= 0 ? [rest.slice(0, idx), rest.slice(idx + 2)] : [rest, '*'];
  }
  return [DUDUCLAW_SERVER, e];
}

function namePatternMatches(pattern: string, name: string): boolean {
  if (!pattern || !name) return false;
  if (pattern.endsWith('*')) {
    const prefix = pattern.slice(0, -1);
    if (!prefix.includes('*')) return name.startsWith(prefix);
  }
  return pattern === name;
}

export function entryMatches(entry: string, tool: string): boolean {
  if (entry.trim() === '*') return true;
  const [es, en] = splitToolRef(entry);
  const [ts, tn] = splitToolRef(tool);
  return es === ts && namePatternMatches(en, tn);
}

export function listMatches(list: ReadonlyArray<string>, tool: string): boolean {
  return list.some((e) => entryMatches(e, tool));
}

/** True when the entry covers more than one tool (`*`, `x*`, `mcp__server`). */
export function isWildcardEntry(entry: string): boolean {
  const [, name] = splitToolRef(entry);
  return entry.trim() === '*' || name === '*' || name.endsWith('*');
}

/**
 * Does `entry` name this catalog tool? DuDuClaw MCP tools use the gateway's
 * matcher. Native Claude tools (`Bash`, `Read`, …) are not served by the
 * duduclaw MCP server, so a `mcp__duduclaw__*` entry must NOT cover them (the
 * Claude CLI would not either); they match by bare name (qualifier ignored) or
 * the catch-all `*`.
 */
function matchesTool(entry: string, tool: BuiltinToolEntry): boolean {
  if (tool.kind === 'claude') {
    if (entry.trim() === '*') return true;
    const e = (entry.split('(')[0] ?? entry).trim();
    return !e.startsWith('mcp__') && e === tool.name;
  }
  return entryMatches(entry, tool.qualified);
}

function listMatchesTool(list: ReadonlyArray<string>, tool: BuiltinToolEntry): boolean {
  return list.some((e) => matchesTool(e, tool));
}

/**
 * An entry the matrix must never delete: anything qualified (`Bash(git:*)`),
 * any glob, or any catch-all. Deleting `Bash(git:*)` from a deny list would
 * widen restricted Bash to full Bash, so such rows are locked (進階設定).
 */
export function isProtectedEntry(entry: string): boolean {
  return entry.includes('(') || entry.includes('*') || isWildcardEntry(entry);
}

function exactMatchesTool(entry: string, tool: BuiltinToolEntry): boolean {
  return !isProtectedEntry(entry) && matchesTool(entry, tool);
}

// ── Per-row state ────────────────────────────────────────────────────────────

export function rowState(tool: BuiltinToolEntry, lists: ToolLists): RowState {
  const deniedHit = lists.denied.filter((e) => matchesTool(e, tool));
  const allowed =
    deniedHit.length === 0 && (lists.allowed.length === 0 || listMatchesTool(lists.allowed, tool));
  const approvalHit = lists.approval.filter((e) => matchesTool(e, tool));
  const ruleAsk = (lists.actionRules ?? []).some(
    (r) => r.verdict === 'ask' && !!r.tool && matchesTool(r.tool, tool),
  );
  return {
    allowed,
    approval: approvalHit.length > 0 || ruleAsk,
    allowLocked: deniedHit.some(isProtectedEntry),
    approvalLocked: ruleAsk || approvalHit.some(isProtectedEntry),
  };
}

/** Catalog rows to show: deprecated aliases only when a list still names them. */
export function visibleTools(tools: ReadonlyArray<BuiltinToolEntry>, lists: ToolLists): BuiltinToolEntry[] {
  return tools.filter(
    (e) =>
      !e.deprecated ||
      listMatchesTool(lists.allowed, e) ||
      listMatchesTool(lists.denied, e) ||
      listMatchesTool(lists.approval, e),
  );
}

// ── Desired state → next lists ───────────────────────────────────────────────

/** `desired` holds ONLY the rows the operator touched, keyed by `qualified`. */
export type DesiredMap = ReadonlyMap<string, Desired>;

export interface NextLists {
  allowed_tools: string[];
  denied_tools: string[];
  approval_required_tools: string[];
}

export interface ChangeItem {
  tool: BuiltinToolEntry;
  allowed?: { from: boolean; to: boolean };
  approval?: { from: boolean; to: boolean };
}

export interface ChangeSummary {
  items: ChangeItem[];
  addedAllowed: number;
  removedAllowed: number;
  approvalChanged: number;
}

/** What actually changes (locked / no-op rows excluded) — drives both the
 *  confirm dialog and the lists written. */
export function summarize(
  tools: ReadonlyArray<BuiltinToolEntry>,
  lists: ToolLists,
  desired: DesiredMap,
): ChangeSummary {
  const items: ChangeItem[] = [];
  let addedAllowed = 0;
  let removedAllowed = 0;
  let approvalChanged = 0;
  for (const tool of tools) {
    const want = desired.get(tool.qualified);
    if (!want) continue;
    const cur = rowState(tool, lists);
    const item: ChangeItem = { tool };
    if (want.allowed !== cur.allowed && !(want.allowed && cur.allowLocked)) {
      item.allowed = { from: cur.allowed, to: want.allowed };
      if (want.allowed) addedAllowed++;
      else removedAllowed++;
    }
    if (want.approval !== cur.approval && !(!want.approval && cur.approvalLocked)) {
      item.approval = { from: cur.approval, to: want.approval };
      approvalChanged++;
    }
    if (item.allowed || item.approval) items.push(item);
  }
  return { items, addedAllowed, removedAllowed, approvalChanged };
}

export function computeNextLists(
  tools: ReadonlyArray<BuiltinToolEntry>,
  lists: ToolLists,
  desired: DesiredMap,
): NextLists {
  let allowed = [...lists.allowed];
  let denied = [...lists.denied];
  let approval = [...lists.approval];
  const { items } = summarize(tools, lists, desired);
  for (const { tool, allowed: a, approval: p } of items) {
    const q = tool.qualified;
    if (a) {
      if (a.to) {
        denied = denied.filter((e) => !exactMatchesTool(e, tool));
        if (allowed.length > 0 && !listMatchesTool(allowed, tool)) allowed.push(q);
      } else if (!denied.includes(q)) {
        denied.push(q);
      }
    }
    if (p) {
      if (p.to) {
        if (!approval.includes(tool.name)) approval.push(tool.name);
      } else {
        approval = approval.filter((e) => !exactMatchesTool(e, tool));
      }
    }
  }
  return { allowed_tools: allowed, denied_tools: denied, approval_required_tools: approval };
}

/** Only the lists that really differ (so an untouched list is never re-sent). */
export function changedLists(lists: ToolLists, next: NextLists): Partial<NextLists> {
  const out: Partial<NextLists> = {};
  const same = (a: ReadonlyArray<string>, b: ReadonlyArray<string>) =>
    a.length === b.length && a.every((v, i) => v === b[i]);
  if (!same(lists.allowed, next.allowed_tools)) out.allowed_tools = next.allowed_tools;
  if (!same(lists.denied, next.denied_tools)) out.denied_tools = next.denied_tools;
  if (!same(lists.approval, next.approval_required_tools)) out.approval_required_tools = next.approval_required_tools;
  return out;
}
