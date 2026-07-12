import type { LicenseSnapshot } from '@/lib/api';

/** Human-readable label for each license tier. */
export const TIER_LABELS: Record<LicenseSnapshot['tier'], string> = {
  opensource: 'Open Source',
  hobby: 'Hobby (Trial)',
  solo: 'Solo',
  studio: 'Studio',
  business: 'Business',
  partner: 'Partner (NFR)',
  personal_pro_self_host: 'Personal Pro',
  self_host_pro: 'Self-Host Pro',
  oem: 'OEM',
};

/** Label for a tier string coming from a loosely-typed RPC (e.g. `about.get`). */
export function tierLabel(tier: string): string {
  return TIER_LABELS[tier as LicenseSnapshot['tier']] ?? tier;
}
