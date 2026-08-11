import { describe, it, expect, beforeEach } from 'vitest';
import {
  useDevPanelStore,
  hasUnseenCritical,
  clampDevPanelHeight,
  DEVPANEL_MIN_HEIGHT,
  DEVPANEL_MAX_HEIGHT,
} from './devpanel-store';

const VISIBILITY_KEY = 'duduclaw-devpanel-visibility';
const HEIGHT_KEY = 'duduclaw-devpanel-height';
const TAB_KEY = 'duduclaw-devpanel-tab';
const LAST_SEEN_CRITICAL_KEY = 'duduclaw-devpanel-critical-seen-at';

function resetStore() {
  localStorage.clear();
  useDevPanelStore.setState({
    visibility: 'collapsed',
    paneHeight: 360,
    activeTab: 'events',
    latestCriticalAt: null,
    lastSeenCriticalAt: null,
  });
}

beforeEach(() => {
  resetStore();
});

describe('devpanel-store — visibility transitions', () => {
  it('defaults to collapsed', () => {
    expect(useDevPanelStore.getState().visibility).toBe('collapsed');
  });

  it('expand/minimize/collapse set and persist visibility', () => {
    useDevPanelStore.getState().expand();
    expect(useDevPanelStore.getState().visibility).toBe('expanded');
    expect(localStorage.getItem(VISIBILITY_KEY)).toBe('expanded');

    useDevPanelStore.getState().minimize();
    expect(useDevPanelStore.getState().visibility).toBe('minimized');
    expect(localStorage.getItem(VISIBILITY_KEY)).toBe('minimized');

    useDevPanelStore.getState().collapse();
    expect(useDevPanelStore.getState().visibility).toBe('collapsed');
    expect(localStorage.getItem(VISIBILITY_KEY)).toBe('collapsed');
  });

  it('toggleQuick goes to expanded from collapsed', () => {
    useDevPanelStore.getState().collapse();
    useDevPanelStore.getState().toggleQuick();
    expect(useDevPanelStore.getState().visibility).toBe('expanded');
  });

  it('toggleQuick goes to expanded from minimized', () => {
    useDevPanelStore.getState().minimize();
    useDevPanelStore.getState().toggleQuick();
    expect(useDevPanelStore.getState().visibility).toBe('expanded');
  });

  it('toggleQuick goes to minimized (never fully collapsed) from expanded', () => {
    useDevPanelStore.getState().expand();
    useDevPanelStore.getState().toggleQuick();
    expect(useDevPanelStore.getState().visibility).toBe('minimized');
  });
});

describe('devpanel-store — pane height', () => {
  it('clamps within [MIN, MAX]', () => {
    expect(clampDevPanelHeight(10)).toBe(DEVPANEL_MIN_HEIGHT);
    expect(clampDevPanelHeight(99999)).toBe(DEVPANEL_MAX_HEIGHT);
    expect(clampDevPanelHeight(400)).toBe(400);
  });

  it('setPaneHeight clamps and persists', () => {
    useDevPanelStore.getState().setPaneHeight(10);
    expect(useDevPanelStore.getState().paneHeight).toBe(DEVPANEL_MIN_HEIGHT);
    expect(localStorage.getItem(HEIGHT_KEY)).toBe(String(DEVPANEL_MIN_HEIGHT));

    useDevPanelStore.getState().setPaneHeight(500);
    expect(useDevPanelStore.getState().paneHeight).toBe(500);
    expect(localStorage.getItem(HEIGHT_KEY)).toBe('500');
  });
});

describe('devpanel-store — active tab', () => {
  it('persists the selected tab', () => {
    useDevPanelStore.getState().setActiveTab('system');
    expect(useDevPanelStore.getState().activeTab).toBe('system');
    expect(localStorage.getItem(TAB_KEY)).toBe('system');
  });
});

describe('devpanel-store — critical alert acknowledgement', () => {
  it('reportCriticalPoll updates latestCriticalAt without persisting it', () => {
    useDevPanelStore.getState().reportCriticalPoll('2026-08-11T00:00:00Z');
    expect(useDevPanelStore.getState().latestCriticalAt).toBe('2026-08-11T00:00:00Z');
    expect(localStorage.getItem('duduclaw-devpanel-latest-critical')).toBeNull();
  });

  it('acknowledgeCritical sets and persists lastSeenCriticalAt to "now"', () => {
    useDevPanelStore.getState().acknowledgeCritical();
    const seen = useDevPanelStore.getState().lastSeenCriticalAt;
    expect(seen).not.toBeNull();
    expect(localStorage.getItem(LAST_SEEN_CRITICAL_KEY)).toBe(seen);
  });
});

describe('hasUnseenCritical', () => {
  it('is false when nothing critical was ever seen', () => {
    expect(hasUnseenCritical({ latestCriticalAt: null, lastSeenCriticalAt: null })).toBe(false);
  });

  it('is true the first time a critical event appears (nothing acknowledged yet)', () => {
    expect(
      hasUnseenCritical({ latestCriticalAt: '2026-08-11T00:00:00Z', lastSeenCriticalAt: null }),
    ).toBe(true);
  });

  it('is false once acknowledged at or after the latest critical timestamp', () => {
    expect(
      hasUnseenCritical({
        latestCriticalAt: '2026-08-11T00:00:00Z',
        lastSeenCriticalAt: '2026-08-11T00:00:01Z',
      }),
    ).toBe(false);
  });

  it('is true again once a NEWER critical event arrives after acknowledgement', () => {
    expect(
      hasUnseenCritical({
        latestCriticalAt: '2026-08-12T00:00:00Z',
        lastSeenCriticalAt: '2026-08-11T00:00:00Z',
      }),
    ).toBe(true);
  });
});
