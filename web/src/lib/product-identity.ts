/** Fixed software identity shared by the dashboard and local chat state. */
export const PRODUCT_NAME = 'DuDuClaw';
export const PRODUCT_LOGO = '🐾';

export function effectiveName(): string { return PRODUCT_NAME; }
export function effectiveLogoGlyph(): string { return PRODUCT_LOGO; }
export function useEffectiveName(): string { return PRODUCT_NAME; }
export function useEffectiveLogoGlyph(): string { return PRODUCT_LOGO; }
export function useEffectiveLogo(): { value: string; isImage: boolean } {
  return { value: PRODUCT_LOGO, isImage: false };
}
