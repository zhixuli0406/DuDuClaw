import { useEffect, useState } from 'react';
import { cn } from '@/lib/utils';
import { BrandMark, type BrandMarkProps } from './BrandMark';
import { useBrandStore } from '@/stores/brand-store';
import { DuDu, type DuDuProps } from '@/components/mascot/DuDu';

/** Follow the resolved theme, including OS changes while in system mode. */
export function useDarkAppearance(): boolean {
  const [dark, setDark] = useState(() => document.documentElement.classList.contains('dark'));
  useEffect(() => {
    const observer = new MutationObserver(() => setDark(document.documentElement.classList.contains('dark')));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
    return () => observer.disconnect();
  }, []);
  return dark;
}

export function ProductLogo({ size = 32, title, className, mono }: BrandMarkProps) {
  const brand = useBrandStore((state) => state.brand);
  const dark = useDarkAppearance();
  if (!brand) return <BrandMark size={size} title={title} className={className} mono={mono} />;
  const boxSize = Math.max(28, size);
  return <img src={(dark && brand.logo_dark) || brand.logo_light} alt={title ?? ''}
    aria-hidden={title ? undefined : true} width={boxSize} height={boxSize}
    style={{ width: boxSize, height: boxSize, objectFit: 'contain' }} className={cn('shrink-0', className)} data-product-logo="" />;
}

/** Keep the original mascot exactly as-is unless a verified brand is effective. */
export function ProductMascot(props: DuDuProps) {
  const brand = useBrandStore((state) => state.brand);
  if (!brand) return <DuDu {...props} />;
  const size = typeof props.size === 'number' ? props.size : ({ sm: 48, md: 96, lg: 160 }[props.size ?? 'md']);
  return <ProductLogo size={size} title={brand.product_name} className={props.className} />;
}
