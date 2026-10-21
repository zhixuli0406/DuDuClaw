import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

// Keep technical attribution deterministic. Arbitrary extra brand fields must not render.
const aboutGet = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    about: { get: (...a: unknown[]) => aboutGet(...a) },
  },
}));

import { AboutPage } from './AboutPage';

describe('<AboutPage>', () => {
  beforeEach(() => {
    aboutGet.mockReset();
  });

  it('renders the fixed upstream-vendor block from the response', async () => {
    aboutGet.mockResolvedValue({
      vendor: {
        name_zh: '嘟嘟數位科技有限公司',
        name_en: 'Dudu Technology Ltd.',
        url: 'https://duduclaw.dudustudio.monster',
      },
      branding: {},
      version: '1.42.0',
      tier: 'oem',
      white_label_active: true,
      source: 'local',
    });

    renderWithProviders(<AboutPage />);

    // Fixed vendor attribution (both language forms) is always present.
    expect(await screen.findByText('嘟嘟數位科技有限公司')).toBeInTheDocument();
    expect(screen.getByText(/Dudu Technology Ltd\./)).toBeInTheDocument();
    // Version surfaced from the response.
    expect(screen.getByText('1.42.0')).toBeInTheDocument();
    expect(screen.getByText('Distributor License')).toBeInTheDocument();
  });

  it('ignores arbitrary branding and About HTML while retaining technical attribution', async () => {
    aboutGet.mockResolvedValue({
      vendor: {
        name_zh: '嘟嘟數位科技有限公司',
        name_en: 'Dudu Technology Ltd.',
        url: 'https://duduclaw.dudustudio.monster',
      },
      // Extra arbitrary fields are ignored, even if a response includes them.
      product_name: 'Other Product',
      branding: {
        about_html: '<h2>Acme Robotics</h2><p>Your friendly automation partner.</p>',
        company_name: '此欄位被 HTML 覆蓋',
      },
      version: '1.42.0',
      tier: 'oem',
      white_label_active: true,
      source: 'bundle',
    });

    renderWithProviders(<AboutPage />);

    // Ignore arbitrary extra brand content in a response.
    expect(await screen.findByText('1.42.0')).toBeInTheDocument();
    expect(screen.queryByText('Acme Robotics')).not.toBeInTheDocument();
    expect(screen.getByText('DuDuClaw')).toBeInTheDocument();
    expect(screen.queryByText('Other Product')).not.toBeInTheDocument();
    expect(screen.queryByText('Your friendly automation partner.')).not.toBeInTheDocument();
    expect(screen.getByRole('link')).toHaveAttribute('href', 'https://duduclaw.dudustudio.monster');
    // Structured custom branding must not render either.
    expect(screen.queryByText('此欄位被 HTML 覆蓋')).not.toBeInTheDocument();
    // The fixed vendor block is still always present.
    expect(screen.getByText('嘟嘟數位科技有限公司')).toBeInTheDocument();
    // The removed branding.about_html never becomes a distributor block.
    expect(document.querySelector('iframe')).toBeNull();
    expect(screen.queryByText('Authorised distributor')).not.toBeInTheDocument();
  });

  it('renders a compiled distributor notice only inside a popups-only sandboxed iframe', async () => {
    aboutGet.mockResolvedValue({
      vendor: {
        name_zh: '嘟嘟數位科技有限公司',
        name_en: 'Dudu Technology Ltd.',
        url: 'https://duduclaw.dudustudio.monster',
      },
      product_name: 'DuDuClaw',
      version: '1.76.0',
      tier: 'oem',
      distributor_notice: '<h2>Example Partner Co.</h2><p>Support line</p>',
    });

    renderWithProviders(<AboutPage />);

    expect(await screen.findByText('Authorised distributor')).toBeInTheDocument();
    expect(screen.getByText('Provided by the authorised distributor')).toBeInTheDocument();
    const frame = document.querySelector('iframe');
    expect(frame).not.toBeNull();
    expect(frame!.getAttribute('sandbox')).toBe('allow-popups allow-popups-to-escape-sandbox');
    expect(frame!.getAttribute('sandbox')).not.toContain('allow-scripts');
    expect(frame!.getAttribute('sandbox')).not.toContain('allow-same-origin');
    expect(frame!.getAttribute('srcdoc')).toContain('<h2>Example Partner Co.</h2><p>Support line</p>');
    // The notice is never injected into the dashboard DOM itself.
    expect(screen.queryByText('Example Partner Co.')).not.toBeInTheDocument();
    // The distributor block sits above the fixed vendor block, which stays intact.
    const vendor = screen.getByText('嘟嘟數位科技有限公司');
    expect(frame!.compareDocumentPosition(vendor) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByRole('link')).toHaveAttribute('href', 'https://duduclaw.dudustudio.monster');
    expect(screen.getByText('DuDuClaw')).toBeInTheDocument();
  });

  it('shows no distributor block when the build has no notice', async () => {
    aboutGet.mockResolvedValue({
      vendor: {
        name_zh: '嘟嘟數位科技有限公司',
        name_en: 'Dudu Technology Ltd.',
        url: 'https://duduclaw.dudustudio.monster',
      },
      product_name: 'DuDuClaw',
      version: '1.76.0',
      tier: 'opensource',
    });

    renderWithProviders(<AboutPage />);

    expect(await screen.findByText('1.76.0')).toBeInTheDocument();
    expect(document.querySelector('iframe')).toBeNull();
    expect(screen.queryByText('Authorised distributor')).not.toBeInTheDocument();
  });

  it('still shows the hard-coded vendor fallback when about.get fails', async () => {
    aboutGet.mockRejectedValue(new Error('offline'));

    renderWithProviders(<AboutPage />);

    // The front-end VENDOR_FALLBACK guarantees attribution even with no RPC.
    expect(await screen.findByText('嘟嘟數位科技有限公司')).toBeInTheDocument();
    // Product identity remains fixed.
    expect(screen.getByText('DuDuClaw')).toBeInTheDocument();
  });
});
