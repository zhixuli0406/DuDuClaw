import { useSystemStore } from '@/stores/system-store';

/**
 * `config.toml [decision] enabled`, as `system.status` reported it — the single
 * place the dashboard decides whether the Decision Lab surface exists.
 *
 * 2026-09-29 audit (X1 落日條款): setting the key `false` makes every
 * `/api/decision/*` route answer 404. Before this hook the SPA kept rendering
 * the nav row, the `/app/system` card and the page itself, so an operator who
 * retired the line still saw an entry that could only fail on contact.
 *
 * Fail **open**: only an explicit `false` counts as off. A gateway too old to
 * carry the field, or a `system.status` that has not landed yet, reads as `true`
 * — a missing field must never make a live feature vanish. That is the right
 * default here because this is presentation of an operator preference, not
 * access control: the gateway's own 404 middleware is the real gate.
 */
export function useDecisionEnabled(): boolean {
  return useSystemStore((s) => s.status?.decision_enabled) !== false;
}
