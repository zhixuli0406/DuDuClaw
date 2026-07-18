import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { CanvasPage } from './CanvasPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ agents: [] });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('CanvasPage', () => {
  it('renders the canvas page header', () => {
    renderWithProviders(<CanvasPage />);
    expect(screen.getByRole('heading', { name: 'Canvas' })).toBeInTheDocument();
  });

  it('exposes the canvas actions menu', () => {
    renderWithProviders(<CanvasPage />);
    expect(screen.getByRole('button', { name: 'Canvas actions' })).toBeInTheDocument();
  });
});
