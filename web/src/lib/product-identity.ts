/** Fixed software identity shared by the dashboard and local chat state. */
export const PRODUCT_NAME = 'DuDuClaw';

/**
 * Default avatar glyph for an AI staff member that has no icon of its own.
 * This is a per-agent avatar placeholder, not the product logo: the product
 * logo is the C1 cat mark, rendered by `<BrandMark>` (components/brand).
 */
export const DEFAULT_AGENT_GLYPH = '🐾';

export function effectiveName(): string { return PRODUCT_NAME; }
export function useEffectiveName(): string { return PRODUCT_NAME; }
