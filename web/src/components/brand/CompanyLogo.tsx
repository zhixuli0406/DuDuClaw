import { useState } from 'react';
import { ProductLogo } from './ProductLogo';
import { useCompanyStore } from '@/stores/company-store';
import { useEffectiveName } from '@/lib/product-identity';

export function useCompanyName(): string {
  const name = useCompanyStore((state) => state.company.name);
  const product = useEffectiveName();
  return name || product;
}

/** Customer identity is display-only; the signed product brand remains separate. */
export function CompanyLogo({ size = 32 }: { size?: number }) {
  const url = useCompanyStore((state) => state.company.logo_url);
  const name = useCompanyName();
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  if (!url || url === failedUrl) return <ProductLogo size={size} title={name} />;
  return <img src={url} alt={name} width={size} height={size}
    style={{ width: size, height: size, objectFit: 'contain' }} className="shrink-0"
    onError={() => setFailedUrl(url)} data-company-logo="" />;
}
