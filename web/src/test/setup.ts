import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterEach, vi } from 'vitest';

afterEach(() => {
  cleanup();
});

// jsdom doesn't implement scrollIntoView
Element.prototype.scrollIntoView = vi.fn();

// jsdom doesn't implement ResizeObserver (react-resizable-panels needs a real
// constructable one for the list+detail split panes on the Inbox / Chat pages).
class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}
vi.stubGlobal('ResizeObserver', ResizeObserverMock);

// jsdom doesn't implement HTMLDialogElement.showModal / close
HTMLDialogElement.prototype.showModal = vi.fn(function (this: HTMLDialogElement) {
  this.setAttribute('open', '');
});
HTMLDialogElement.prototype.close = vi.fn(function (this: HTMLDialogElement) {
  this.removeAttribute('open');
});

// Mock WebSocket globally
vi.stubGlobal(
  'WebSocket',
  vi.fn(() => ({
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    close: vi.fn(),
    send: vi.fn(),
    readyState: 1,
  }))
);

// This jsdom build exposes a `localStorage` whose methods are absent/throwing,
// which is why production code guards every access in try/catch and why tests
// that exercise persistence would otherwise fail. Install a deterministic
// in-memory Storage so persistence-dependent tests run reliably.
function createMemoryStorage(): Storage {
  let store: Record<string, string> = {};
  return {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => {
      store[k] = String(v);
    },
    removeItem: (k: string) => {
      delete store[k];
    },
    clear: () => {
      store = {};
    },
    key: (i: number) => Object.keys(store)[i] ?? null,
    get length() {
      return Object.keys(store).length;
    },
  } as Storage;
}
vi.stubGlobal('localStorage', createMemoryStorage());
