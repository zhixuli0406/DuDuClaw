import { useBrandStore } from '@/stores/brand-store';

/** Original product identity used whenever no verified distributor brand is effective. */
export const PRODUCT_NAME = 'DuDuClaw';

/**
 * Default avatar glyph for an AI staff member that has no icon of its own.
 * This is a per-agent avatar placeholder, not the product logo: the product
 * logo is the C1 cat mark, rendered by `<BrandMark>` (components/brand).
 */
export const DEFAULT_AGENT_GLYPH = '🐾';

export function effectiveName(): string { return useBrandStore.getState().brand?.product_name ?? PRODUCT_NAME; }
export function useEffectiveName(): string { return useBrandStore((state) => state.brand?.product_name ?? PRODUCT_NAME); }
