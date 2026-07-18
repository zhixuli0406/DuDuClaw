import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ReportPage } from './ReportPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('ReportPage', () => {
  it('renders the report header and the period range control', () => {
    renderWithProviders(<ReportPage />);
    expect(screen.getByRole('heading', { name: 'Reports' })).toBeInTheDocument();
    // Segmented period control (day / week / month).
    expect(screen.getByRole('radio', { name: 'Month' })).toBeInTheDocument();
  });
});
