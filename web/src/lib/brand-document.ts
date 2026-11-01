import type { DistributorBrand } from '@/stores/brand-store';

/** Returns a disposer that restores the original document identity. */
export function applyDocumentBrand(brand: DistributorBrand | null, dark: boolean): () => void {
  if (!brand) return () => {};
  const title = document.title;
  const icons = Array.from(document.querySelectorAll<HTMLLinkElement>('link[rel="icon"]'));
  const attributes = icons.map((icon) => ({ icon, href: icon.getAttribute('href'), type: icon.getAttribute('type') }));
  // Layouts own the page prefix; public pages keep their existing title too.
  const alreadyBranded = title === brand.product_name || title === `${brand.product_name} Dashboard`
    || title.endsWith(` · ${brand.product_name}`);
  const brandedTitle = alreadyBranded ? title : title === 'DuDuClaw Dashboard'
    ? `${brand.product_name} Dashboard`
    : title === 'DuDuClaw' ? brand.product_name
      : title.endsWith(' · DuDuClaw') ? `${title.slice(0, -'DuDuClaw'.length)}${brand.product_name}` : title;
  document.title = brandedTitle;
  const logo = (dark && brand.logo_dark) || brand.logo_light;
  for (const icon of icons) {
    icon.href = logo;
    icon.type = logo.substring(5, logo.indexOf(';'));
  }
  return () => {
    if (document.title === brandedTitle) document.title = title;
    for (const { icon, href, type } of attributes) {
      if (href === null) icon.removeAttribute('href'); else icon.setAttribute('href', href);
      if (type === null) icon.removeAttribute('type'); else icon.setAttribute('type', type);
    }
  };
}
