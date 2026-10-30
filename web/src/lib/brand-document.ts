import type { DistributorBrand } from '@/stores/brand-store';

/** Returns a disposer that restores the original document identity. */
export function applyDocumentBrand(brand: DistributorBrand | null, dark: boolean): () => void {
  if (!brand) return () => {};
  const title = document.title;
  const icons = Array.from(document.querySelectorAll<HTMLLinkElement>('link[rel="icon"]'));
  const attributes = icons.map((icon) => ({ icon, href: icon.getAttribute('href'), type: icon.getAttribute('type') }));
  document.title = brand.product_name;
  const logo = (dark && brand.logo_dark) || brand.logo_light;
  for (const icon of icons) {
    icon.href = logo;
    icon.type = logo.substring(5, logo.indexOf(';'));
  }
  return () => {
    document.title = title;
    for (const { icon, href, type } of attributes) {
      if (href === null) icon.removeAttribute('href'); else icon.setAttribute('href', href);
      if (type === null) icon.removeAttribute('type'); else icon.setAttribute('type', type);
    }
  };
}
