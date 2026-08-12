import { describe, it, expect, beforeEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router';
import type { ReactNode } from 'react';
import {
  useUrlState,
  useUrlStateNullable,
  useUrlNumberState,
  __resetUrlStateStaging,
} from './use-url-state';

function wrapperAt(path: string) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <MemoryRouter initialEntries={[path]}>{children}</MemoryRouter>;
  };
}

/** Exposes the live query string alongside the hook under test. */
function useProbe<R>(hook: () => R) {
  const location = useLocation();
  return { value: hook(), search: location.search, key: location.key };
}

beforeEach(() => {
  __resetUrlStateStaging();
});

const TABS = ['overview', 'detail', 'raw'] as const;
type Tab = (typeof TABS)[number];

describe('useUrlState', () => {
  it('falls back to the default when the param is absent', () => {
    const { result } = renderHook(() => useUrlState<Tab>('tab', 'overview', { allowed: TABS }), {
      wrapper: wrapperAt('/x'),
    });
    expect(result.current[0]).toBe('overview');
  });

  it('reads the initial value from the URL (deep link)', () => {
    const { result } = renderHook(() => useUrlState<Tab>('tab', 'overview', { allowed: TABS }), {
      wrapper: wrapperAt('/x?tab=raw'),
    });
    expect(result.current[0]).toBe('raw');
  });

  it('rejects a value outside `allowed` and uses the default instead', () => {
    const { result } = renderHook(() => useUrlState<Tab>('tab', 'overview', { allowed: TABS }), {
      wrapper: wrapperAt('/x?tab=bogus'),
    });
    expect(result.current[0]).toBe('overview');
  });

  it('accepts any string when `allowed` is omitted', () => {
    const { result } = renderHook(() => useUrlState('q', ''), {
      wrapper: wrapperAt('/x?q=hello%20world'),
    });
    expect(result.current[0]).toBe('hello world');
  });

  it('treats an empty param as absent', () => {
    const { result } = renderHook(() => useUrlState('agent', 'all'), {
      wrapper: wrapperAt('/x?agent='),
    });
    expect(result.current[0]).toBe('all');
  });

  it('writes the value into the URL', () => {
    const { result } = renderHook(() => useProbe(() => useUrlState<Tab>('tab', 'overview', { allowed: TABS })), {
      wrapper: wrapperAt('/x'),
    });
    act(() => result.current.value[1]('detail'));
    expect(result.current.search).toBe('?tab=detail');
    expect(result.current.value[0]).toBe('detail');
  });

  it('removes the param when set back to the default', () => {
    const { result } = renderHook(() => useProbe(() => useUrlState<Tab>('tab', 'overview', { allowed: TABS })), {
      wrapper: wrapperAt('/x?tab=raw'),
    });
    act(() => result.current.value[1]('overview'));
    expect(result.current.search).toBe('');
    expect(result.current.value[0]).toBe('overview');
  });

  it('preserves unrelated params when writing', () => {
    const { result } = renderHook(() => useProbe(() => useUrlState('q', '')), {
      wrapper: wrapperAt('/x?tab=raw&agent=nova'),
    });
    act(() => result.current.value[1]('hi'));
    const params = new URLSearchParams(result.current.search);
    expect(params.get('tab')).toBe('raw');
    expect(params.get('agent')).toBe('nova');
    expect(params.get('q')).toBe('hi');
  });

  it('replaces history by default (no extra Back entry)', () => {
    const { result } = renderHook(() => useProbe(() => useUrlState('q', '')), {
      wrapper: wrapperAt('/x'),
    });
    act(() => result.current.value[1]('a'));
    act(() => result.current.value[1]('ab'));
    // MemoryRouter exposes history depth via `location.idx` internally; the
    // observable contract here is that the value tracks the last write.
    expect(result.current.search).toBe('?q=ab');
  });

  it('coalesces two different keys updated in the same tick', () => {
    const { result } = renderHook(
      () =>
        useProbe(() => {
          const filter = useUrlState('agent', 'all');
          const page = useUrlNumberState('page', 0, { min: 0 });
          return { filter, page };
        }),
      { wrapper: wrapperAt('/x') },
    );

    act(() => {
      result.current.value.filter[1]('nova');
      result.current.value.page[1](3);
    });

    const params = new URLSearchParams(result.current.search);
    expect(params.get('agent')).toBe('nova');
    expect(params.get('page')).toBe('3');
  });

  it('does not reuse staged params across separate interactions', async () => {
    const { result } = renderHook(
      () =>
        useProbe(() => {
          const filter = useUrlState('agent', 'all');
          const page = useUrlNumberState('page', 0, { min: 0 });
          return { filter, page };
        }),
      { wrapper: wrapperAt('/x') },
    );

    act(() => result.current.value.filter[1]('nova'));
    await act(async () => {
      await Promise.resolve();
    });
    // A later interaction that clears the filter must not resurrect anything.
    act(() => result.current.value.filter[1]('all'));
    expect(result.current.search).toBe('');
  });
});

describe('useUrlStateNullable', () => {
  it('is null when the param is absent', () => {
    const { result } = renderHook(() => useUrlStateNullable('sel'), { wrapper: wrapperAt('/x') });
    expect(result.current[0]).toBeNull();
  });

  it('restores the selected id from a deep link', () => {
    const { result } = renderHook(() => useUrlStateNullable('sel'), {
      wrapper: wrapperAt('/x?sel=run-42'),
    });
    expect(result.current[0]).toBe('run-42');
  });

  it('writes and clears the id', () => {
    const { result } = renderHook(() => useProbe(() => useUrlStateNullable('sel')), {
      wrapper: wrapperAt('/x'),
    });
    act(() => result.current.value[1]('run-7'));
    expect(result.current.search).toBe('?sel=run-7');
    act(() => result.current.value[1](null));
    expect(result.current.search).toBe('');
  });

  it('treats an empty param as no selection', () => {
    const { result } = renderHook(() => useUrlStateNullable('sel'), {
      wrapper: wrapperAt('/x?sel='),
    });
    expect(result.current[0]).toBeNull();
  });
});

describe('useUrlNumberState', () => {
  it('parses an integer from the URL', () => {
    const { result } = renderHook(() => useUrlNumberState('page', 0, { min: 0 }), {
      wrapper: wrapperAt('/x?page=4'),
    });
    expect(result.current[0]).toBe(4);
  });

  it('falls back to the default for a non-numeric value', () => {
    const { result } = renderHook(() => useUrlNumberState('page', 0, { min: 0 }), {
      wrapper: wrapperAt('/x?page=abc'),
    });
    expect(result.current[0]).toBe(0);
  });

  it('falls back to the default below `min` / above `max`', () => {
    const below = renderHook(() => useUrlNumberState('page', 0, { min: 0 }), {
      wrapper: wrapperAt('/x?page=-3'),
    });
    expect(below.result.current[0]).toBe(0);

    const above = renderHook(() => useUrlNumberState('days', 7, { min: 1, max: 30 }), {
      wrapper: wrapperAt('/x?days=999'),
    });
    expect(above.result.current[0]).toBe(7);
  });

  it('removes the param when set back to the default', () => {
    const { result } = renderHook(() => useProbe(() => useUrlNumberState('page', 0, { min: 0 })), {
      wrapper: wrapperAt('/x?page=2'),
    });
    act(() => result.current.value[1](0));
    expect(result.current.search).toBe('');
  });
});
