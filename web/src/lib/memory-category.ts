/**
 * Topic classification for memory entries — the grouping the memory page
 * renders (2026-07-30 client feedback: "I like how Perplexity shows memory",
 * i.e. a category rail on the left and per-category cards on the right).
 *
 * The backend stores no topic field: a memory carries `layer`, `tags`,
 * `source_event` and free-form content. Rather than adding an LLM pass (a
 * per-entry cost on a list that renders on every page view), classification is
 * deterministic and runs in two ordered passes:
 *
 *   1. **Origin** — `source_event` / `tags` written by a known pipeline
 *      (footprint distillation, reflexion rules, decision capture, ...). These
 *      are exact matches and always win: a footprint roll-up must never be
 *      keyword-matched into "工作與專案" because it happens to name an app.
 *   2. **Content keywords** — bilingual term lists scored over the entry text.
 *      Highest score wins; a tie falls back to the earlier category in
 *      `MEMORY_CATEGORIES` order so the result is stable across renders.
 *
 * Matching is CJK-safe: CJK terms are matched as plain substrings (Chinese has
 * no word boundaries), ASCII terms require a word boundary so `hr` does not
 * match inside `there`. Nothing here slices strings by byte index.
 */

/** Stable category ids. `other` is the guaranteed fallback. */
export type MemoryCategoryId =
  | 'work'
  | 'people'
  | 'business'
  | 'preferences'
  | 'rules'
  | 'tools'
  | 'schedule'
  | 'finance'
  | 'footprint'
  | 'learning'
  | 'other';

export interface MemoryCategory {
  id: MemoryCategoryId;
  /** i18n message id for the display name (`memory.category.<id>`). */
  label: string;
  /** Lucide icon name, resolved by the page to keep this module UI-free. */
  icon: string;
}

/**
 * Display order of the category rail. Origin-derived buckets (`footprint`,
 * `learning`) sit near the end because they are machine telemetry, not things
 * the user told the agent.
 */
export const MEMORY_CATEGORIES: readonly MemoryCategory[] = [
  { id: 'work', label: 'memory.category.work', icon: 'Briefcase' },
  { id: 'people', label: 'memory.category.people', icon: 'Users' },
  { id: 'business', label: 'memory.category.business', icon: 'Handshake' },
  { id: 'preferences', label: 'memory.category.preferences', icon: 'Heart' },
  { id: 'rules', label: 'memory.category.rules', icon: 'ScrollText' },
  { id: 'tools', label: 'memory.category.tools', icon: 'Wrench' },
  { id: 'schedule', label: 'memory.category.schedule', icon: 'CalendarClock' },
  { id: 'finance', label: 'memory.category.finance', icon: 'Wallet' },
  { id: 'footprint', label: 'memory.category.footprint', icon: 'Activity' },
  { id: 'learning', label: 'memory.category.learning', icon: 'Sparkles' },
  { id: 'other', label: 'memory.category.other', icon: 'Boxes' },
] as const;

/**
 * Pass 1 — exact origin mapping. Keys are the `source_event` constants written
 * by the Rust pipelines (`footprint_distill.rs`, `wiki_ingest.rs`,
 * `rule_lifecycle.rs`, `decision_capture.rs`, `user_profile.rs`, `night.rs`).
 * Unlisted origins fall through to keyword scoring.
 */
const ORIGIN_CATEGORY: Readonly<Record<string, MemoryCategoryId>> = {
  footprint_distill: 'footprint',
  prediction_episodic: 'learning',
  reflexion_consolidation: 'rules',
  persona_suppression_induction: 'preferences',
  user_profile: 'preferences',
  user_profile_consolidation: 'preferences',
  decision_capture: 'rules',
  decision_resolve: 'rules',
  agent_mood: 'learning',
};

/** Pass 1 (tags variant) — same idea, for entries whose origin lives in tags. */
const TAG_CATEGORY: Readonly<Record<string, MemoryCategoryId>> = {
  'footprint-distill': 'footprint',
  'user-profile': 'preferences',
  'profile-summary': 'preferences',
  reflexion: 'rules',
  'probation-rule': 'rules',
  'retired-rule': 'rules',
  decision: 'rules',
};

/**
 * Pass 2 — keyword lists per category, Traditional Chinese first then English.
 * Kept deliberately small and concrete: a term earns its place only if seeing
 * it in a sentence genuinely implies the category.
 */
const KEYWORDS: Readonly<Record<Exclude<MemoryCategoryId, 'other'>, readonly string[]>> = {
  work: [
    '專案', 'project', '任務', '需求', '進度', '交付', '上線', '版本', '里程碑',
    '會議', '簡報', '報告', '文件', 'deadline', 'sprint', 'milestone', 'roadmap',
  ],
  people: [
    '同事', '主管', '老闆', '員工', '團隊', '部門', '聯絡', '電話', 'email', '信箱',
    '負責人', '窗口', 'contact', 'colleague', 'manager',
  ],
  business: [
    '客戶', '合約', '報價', '訂單', '成交', '簽約', '經銷', '合作', '供應商',
    '售後', '客訴', '提案', 'customer', 'client', 'contract', 'quote', 'order', 'vendor',
  ],
  preferences: [
    '喜歡', '偏好', '習慣', '不喜歡', '討厭', '風格', '語氣', '稱呼', '慣用',
    '口味', 'prefer', 'favourite', 'favorite', 'dislike', 'style', 'tone',
  ],
  rules: [
    '規則', '政策', '規定', '流程', 'sop', '標準', '禁止', '必須', '一律', '原則',
    '決策', '守則', 'policy', 'rule', 'process', 'workflow', 'guideline', 'must', 'never',
  ],
  tools: [
    '系統', '工具', '帳號', '密碼', '設定', '安裝', '部署', '伺服器', '資料庫',
    '網址', 'api', 'server', 'database', 'deploy', 'config', 'repo', 'github', 'docker',
  ],
  schedule: [
    '時間', '行程', '排程', '每週', '每天', '每月', '截止', '日期', '地址', '地點',
    '辦公室', '會面', 'schedule', 'weekly', 'daily', 'monthly', 'address', 'office',
  ],
  finance: [
    '費用', '價格', '金額', '付款', '發票', '報帳', '預算', '成本', '薪資', '收款',
    '匯款', '稅', 'invoice', 'payment', 'budget', 'cost', 'price', 'billing', 'refund',
  ],
  footprint: ['使用時長', '活躍時段', '前景應用程式'],
  // WP15: telemetry no longer reaches this list at all — the gateway returns it
  // as a separate `signals` array. Only the unmistakable marker is kept, as a
  // safety net for an older gateway that still serves one merged list; the
  // former generic terms ('預期', '滿意度') were dropped because they mis-filed
  // ordinary memories ("客戶滿意度調查在下週") as machine learning signals.
  learning: ['prediction deviation', '學習訊號'],
};

/**
 * True when the term contains any CJK codepoint (no word boundaries apply).
 * Covers the kana blocks (the dashboard also ships ja-JP), unified ideographs
 * + extension A, and compatibility ideographs.
 */
const CJK_RE = /[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/;

function isCjk(term: string): boolean {
  return CJK_RE.test(term);
}

/**
 * Whole-word (ASCII) / substring (CJK) case-insensitive containment. Mirrors
 * `duduclaw_core::word_contains_ci` — an unanchored `includes` would let `api`
 * match inside `rapid` and mis-file the entry.
 */
export function termMatches(haystackLower: string, term: string): boolean {
  const t = term.toLowerCase();
  if (!t) return false;
  if (isCjk(t)) return haystackLower.includes(t);
  const idx = haystackLower.indexOf(t);
  if (idx === -1) return false;
  // Scan every occurrence — the first may be embedded while a later one is not.
  for (let at = idx; at !== -1; at = haystackLower.indexOf(t, at + 1)) {
    const before = at === 0 ? '' : haystackLower[at - 1];
    const after = haystackLower[at + t.length] ?? '';
    const boundary = (c: string) => c === '' || !/[a-z0-9]/.test(c);
    if (boundary(before) && boundary(after)) return true;
  }
  return false;
}

/** The shape `classifyMemory` needs — a structural subset of `MemoryEntry`. */
export interface ClassifiableMemory {
  content: string;
  tags?: readonly string[];
  source_event?: string;
}

/**
 * Assign one entry to a category. Origin wins over content; content ties break
 * toward the earlier category in {@link MEMORY_CATEGORIES}; no signal at all
 * yields `other`.
 */
export function classifyMemory(entry: ClassifiableMemory): MemoryCategoryId {
  const origin = (entry.source_event ?? '').trim().toLowerCase();
  if (origin) {
    const hit = ORIGIN_CATEGORY[origin];
    if (hit) return hit;
    // `migrate-from-<platform>` and friends carry a stable prefix.
    if (origin.startsWith('migrate-from-')) return 'other';
  }

  for (const tag of entry.tags ?? []) {
    const hit = TAG_CATEGORY[tag.trim().toLowerCase()];
    if (hit) return hit;
  }

  const text = entry.content.toLowerCase();
  let best: MemoryCategoryId = 'other';
  let bestScore = 0;
  for (const cat of MEMORY_CATEGORIES) {
    if (cat.id === 'other') continue;
    const terms = KEYWORDS[cat.id];
    let score = 0;
    for (const term of terms) {
      if (termMatches(text, term)) score += 1;
    }
    if (score > bestScore) {
      bestScore = score;
      best = cat.id;
    }
  }
  return best;
}

/** One category plus the entries that landed in it. */
export interface CategoryBucket<T> {
  category: MemoryCategory;
  entries: T[];
}

/**
 * Group entries into category buckets in {@link MEMORY_CATEGORIES} order.
 * Empty categories are dropped so the rail only lists what actually exists.
 * Entry order within a bucket is preserved (the backend already sorts newest
 * first).
 */
export function groupByCategory<T extends ClassifiableMemory>(
  entries: readonly T[],
): CategoryBucket<T>[] {
  const buckets = new Map<MemoryCategoryId, T[]>();
  for (const entry of entries) {
    const id = classifyMemory(entry);
    const list = buckets.get(id);
    if (list) list.push(entry);
    else buckets.set(id, [entry]);
  }
  return MEMORY_CATEGORIES.filter((c) => buckets.has(c.id)).map((category) => ({
    category,
    entries: buckets.get(category.id) ?? [],
  }));
}
