import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WidgetComposerPage } from './WidgetComposerPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('WidgetComposerPage', () => {
  it('renders the breadcrumb header and the empty preview', () => {
    renderWithProviders(<WidgetComposerPage />);
    // Breadcrumb root segment (widgets.title) + guided authoring surface.
    expect(screen.getByText('Widget Studio')).toBeInTheDocument();
    expect(
      screen.getByText(
        'Generate or paste HTML and it renders here in the same sandbox as the dashboard',
      ),
    ).toBeInTheDocument();
  });
});
