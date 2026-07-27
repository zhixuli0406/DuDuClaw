import { describe, it, expect, afterEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { PetStudioPage } from './PetStudioPage';

const renderPage = () =>
  renderWithProviders(
    <SidebarProvider>
      <PetStudioPage />
    </SidebarProvider>
  );

describe('PetStudioPage', () => {
  afterEach(() => {
    // Clean up any injected Tauri global between tests.
    delete (window as unknown as { __TAURI__?: unknown }).__TAURI__;
    vi.restoreAllMocks();
  });

  it('shows the desktop-only empty state outside the Tauri shell', () => {
    renderPage();
    expect(screen.getByText('Desktop app only')).toBeInTheDocument();
    // The creation card must NOT render in a plain browser.
    expect(screen.queryByText('Create a pet')).not.toBeInTheDocument();
  });

  it('renders the studio when running inside Tauri', async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === 'pet_list') return [];
      if (cmd === 'pet_model_status')
        return {
          birefnetPresent: true,
          siluetaPresent: false,
          birefnetUrl: 'https://example/b',
          siluetaUrl: 'https://example/s',
        };
      return null;
    });
    (window as unknown as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };

    renderPage();
    // The upload/generate card renders in the desktop shell.
    expect(await screen.findByText('Create a pet')).toBeInTheDocument();
    // pet_list + pet_model_status are queried on mount.
    expect(invoke).toHaveBeenCalledWith('pet_list', undefined);
    expect(invoke).toHaveBeenCalledWith('pet_model_status', undefined);
  });
});
