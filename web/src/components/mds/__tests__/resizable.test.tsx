import { describe, it, expect, beforeAll, vi } from 'vitest';
import { renderWithProviders } from '@/test/render';
import {
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
} from '../resizable';

// react-resizable-panels v4 constructs a ResizeObserver on mount, which jsdom
// does not implement. Provide a no-op stub so the group can render.
beforeAll(() => {
  if (typeof globalThis.ResizeObserver === 'undefined') {
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
  }
});

describe('Resizable', () => {
  it('renders a group of panels split by a handle', () => {
    const { container } = renderWithProviders(
      <ResizablePanelGroup orientation="horizontal">
        <ResizablePanel defaultSize={320} minSize={240}>
          Left
        </ResizablePanel>
        <ResizableHandle />
        <ResizablePanel>Right</ResizablePanel>
      </ResizablePanelGroup>
    );
    expect(
      container.querySelector('[data-slot="resizable-panel-group"]')
    ).toBeInTheDocument();
    expect(
      container.querySelectorAll('[data-slot="resizable-panel"]').length
    ).toBe(2);
    const handle = container.querySelector('[data-slot="resizable-handle"]')!;
    expect(handle).toHaveClass('bg-surface-border');
  });
});
