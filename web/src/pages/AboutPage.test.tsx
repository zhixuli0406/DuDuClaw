import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

// Mock the API so about.get is deterministic. branding.ts only touches `api`
// inside its (unused-here) fetch(), so a minimal shape is safe.
const aboutGet = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    about: { get: (...a: unknown[]) => aboutGet(...a) },
    branding: { get: vi.fn() },
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
        name_en: 'DuDu Digital Technology Co., Ltd.',
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
    expect(screen.getByText(/DuDu Digital Technology Co\., Ltd\./)).toBeInTheDocument();
    // Version surfaced from the response.
    expect(screen.getByText('1.42.0')).toBeInTheDocument();
  });

  it('renders the sanitized about_html block when present', async () => {
    aboutGet.mockResolvedValue({
      vendor: {
        name_zh: '嘟嘟數位科技有限公司',
        name_en: 'DuDu Digital Technology Co., Ltd.',
        url: 'https://duduclaw.dudustudio.monster',
      },
      // Backend-sanitized HTML block (structured fields are ignored when set).
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

    // HTML block content renders…
    expect(await screen.findByText('Acme Robotics')).toBeInTheDocument();
    expect(screen.getByText('Your friendly automation partner.')).toBeInTheDocument();
    // …and the structured company field is suppressed in favor of the HTML.
    expect(screen.queryByText('此欄位被 HTML 覆蓋')).not.toBeInTheDocument();
    // The fixed vendor block is still always present.
    expect(screen.getByText('嘟嘟數位科技有限公司')).toBeInTheDocument();
  });

  it('still shows the hard-coded vendor fallback when about.get fails', async () => {
    aboutGet.mockRejectedValue(new Error('offline'));

    renderWithProviders(<AboutPage />);

    // The front-end VENDOR_FALLBACK guarantees attribution even with no RPC.
    expect(await screen.findByText('嘟嘟數位科技有限公司')).toBeInTheDocument();
    // Default product name when no white-label branding is set.
    expect(screen.getByText('DuDuClaw')).toBeInTheDocument();
  });
});
