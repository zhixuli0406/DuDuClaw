import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WidgetsPage } from './WidgetsPage';

beforeEach(() => {
  vi.clearAllMocks();
  // Covers both widgets.custom.list (.widgets/.max_per_user) and
  // dashboard.layoutGet (.layout) since every RPC resolves to this shape.
  mockWsClient.call.mockResolvedValue({ widgets: [], max_per_user: 0, layout: null });
});

describe('WidgetsPage', () => {
  it('renders the collection header and the gallery tabs', () => {
    renderWithProviders(<WidgetsPage />);
    expect(screen.getByRole('heading', { name: 'Widget Studio' })).toBeInTheDocument();
    // Line tabs (mine / shared).
    expect(screen.getByRole('tab', { name: /Mine/ })).toBeInTheDocument();
  });
});
