import { afterEach, describe, expect, it } from 'vitest';
import { render, cleanup, act } from '@testing-library/react';
import { ProductLogo, ProductMascot } from './ProductLogo';
import { useBrandStore } from '@/stores/brand-store';
const light = 'data:image/png;base64,iVBORw0KGgo=';
const dark = 'data:image/webp;base64,UklGRgAAAABXRUJQ';
afterEach(() => { cleanup(); useBrandStore.getState().setBrand(null); document.documentElement.classList.remove('dark'); });
describe('ProductLogo', () => {
  it('preserves the original BrandMark and mascot without a brand', () => {
    const { container } = render(<><ProductLogo /><ProductMascot face="idle" animated={false} /></>);
    expect(container.querySelector('[data-brand-mark]')).not.toBeNull();
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelectorAll('svg')).toHaveLength(2);
  });
  it('contains arbitrary aspect ratios and uses the theme-specific logo', async () => {
    useBrandStore.getState().setBrand({ product_name: 'Partner', logo_light: light, logo_dark: dark });
    const { getByRole } = render(<ProductLogo size={64} title="Partner" />);
    expect(getByRole('img')).toHaveAttribute('src', light);
    expect(getByRole('img')).toHaveStyle({ objectFit: 'contain', width: '64px', height: '64px' });
    await act(async () => { document.documentElement.classList.add('dark'); });
    expect(getByRole('img')).toHaveAttribute('src', dark);
  });
  it('replaces the mascot and falls back to light when dark has no variant', () => {
    useBrandStore.getState().setBrand({ product_name: 'Partner', logo_light: light });
    document.documentElement.classList.add('dark');
    const { getByRole, container } = render(<ProductMascot size="lg" face="celebrating" />);
    expect(getByRole('img')).toHaveAttribute('src', light);
    expect(getByRole('img')).toHaveAttribute('width', '160');
    expect(container.querySelector('svg')).toBeNull();
  });
});
