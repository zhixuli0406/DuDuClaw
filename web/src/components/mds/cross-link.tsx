import { ArrowRight } from 'lucide-react';

/**
 * CrossLink — R-EDIT-SOURCE primitive (12-ia-redesign-blueprint.md §1): the
 * "the real switch for this lives over there" affordance that belongs next
 * to every read-only mirror field or adjacent-concept block across the
 * dashboard. Plain text plus an arrow, deliberately quiet so it reads as
 * guidance, not another control competing with the fields above it.
 *
 * `EditAgentPage.tsx` introduced this exact shape locally for WP-C (§2-2,
 * account-pool / budget cross-links). Promoted to `mds` here so every other
 * read-only-mirror page (WP-D, §2-3/4/5/7/9/11/12/14) shares one definition
 * instead of re-implementing the same markup per page — `EditAgentPage.tsx`
 * itself is left untouched (out of scope for WP-D) and keeps its own local
 * copy; the two are visually identical by construction.
 */
export function CrossLink({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="inline-flex items-center gap-1 text-xs font-medium text-brand underline-offset-2 transition-colors hover:underline"
    >
      {label}
      <ArrowRight className="size-3.5" />
    </button>
  );
}
