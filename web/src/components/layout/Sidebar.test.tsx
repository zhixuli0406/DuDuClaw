import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { Sidebar } from './Sidebar';
import { useAuthStore } from '@/stores/auth-store';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useAuthStore.setState({ user: { display_name: 'A', role: 'admin' } as never });
});

describe('Sidebar', () => {
  // v2 "嘟嘟事務所" IA (§4.2): daily items are flat (no header); the collapsible
  // sections are 工作 / 員工 / 公司. Home is the single spine.
  it('renders the flat daily items and the collapsible sections', () => {
    renderWithProviders(<Sidebar />);
    // Flat daily items (rendered without a section header).
    expect(screen.getByRole('link', { name: /Home/i })).toBeInTheDocument();
    // Collapsible section headers.
    expect(screen.getByText(/^Work$/)).toBeInTheDocument();
    expect(screen.getByText(/^Company$/)).toBeInTheDocument();
    // The primary create-task action.
    expect(screen.getByRole('button', { name: /New task/i })).toBeInTheDocument();
  });
});
