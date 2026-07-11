import { describe, it, expect } from 'vitest';
import { screen, fireEvent, act } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { PanelProvider, PropertiesPanel, usePanel } from './PropertiesPanel';

function Injector() {
  const { setPanel } = usePanel();
  return (
    <button type="button" onClick={() => setPanel({ title: '任務屬性', content: <p>面板內容</p> })}>
      inject
    </button>
  );
}

describe('<PropertiesPanel> + usePanel', () => {
  it('injected content renders in the panel', () => {
    renderWithProviders(
      <PanelProvider>
        <Injector />
        <PropertiesPanel />
      </PanelProvider>,
    );
    act(() => {
      fireEvent.click(screen.getByText('inject'));
    });
    expect(screen.getByText('任務屬性')).toBeInTheDocument();
    expect(screen.getByText('面板內容')).toBeInTheDocument();
  });

  it('collapse toggle hides the body and persists the preference', () => {
    renderWithProviders(
      <PanelProvider>
        <Injector />
        <PropertiesPanel />
      </PanelProvider>,
    );
    act(() => {
      fireEvent.click(screen.getByText('inject'));
    });
    // Collapse via the header button.
    fireEvent.click(screen.getByRole('button', { name: 'Collapse panel' }));
    expect(screen.queryByText('面板內容')).not.toBeInTheDocument();
    expect(localStorage.getItem('duduclaw:ui:panel-collapsed')).toBe('1');
    // Expand again.
    fireEvent.click(screen.getByRole('button', { name: 'Expand panel' }));
    expect(screen.getByText('面板內容')).toBeInTheDocument();
  });

  it('usePanel is safe (no-op) outside a provider', () => {
    function Lone() {
      const { collapsed, setPanel } = usePanel();
      return <span>{`c:${collapsed}:${typeof setPanel}`}</span>;
    }
    renderWithProviders(<Lone />);
    expect(screen.getByText('c:false:function')).toBeInTheDocument();
  });
});
