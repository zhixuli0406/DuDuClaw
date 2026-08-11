import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  hasUnsavedFormInput,
  parseNavigatePath,
  handleDashboardNavigate,
  __resetDashboardNavigateCooldownForTest,
  NAVIGATE_COOLDOWN_MS,
} from './dashboard-navigate';

beforeEach(() => {
  __resetDashboardNavigateCooldownForTest();
  document.body.innerHTML = '';
});

describe('parseNavigatePath', () => {
  it('accepts a same-origin relative path', () => {
    expect(parseNavigatePath({ path: '/inbox?item=ap-1' })).toBe('/inbox?item=ap-1');
    expect(parseNavigatePath({ path: '/agents' })).toBe('/agents');
  });

  it('rejects a protocol-relative path (open-redirect shape)', () => {
    expect(parseNavigatePath({ path: '//evil.com' })).toBeNull();
  });

  it('rejects a path with no leading slash', () => {
    expect(parseNavigatePath({ path: 'inbox' })).toBeNull();
  });

  it('rejects non-string / missing / malformed payloads', () => {
    expect(parseNavigatePath(null)).toBeNull();
    expect(parseNavigatePath(undefined)).toBeNull();
    expect(parseNavigatePath({})).toBeNull();
    expect(parseNavigatePath({ path: 42 })).toBeNull();
    expect(parseNavigatePath('/inbox')).toBeNull();
  });

  it('rejects an empty or over-long path', () => {
    expect(parseNavigatePath({ path: '' })).toBeNull();
    expect(parseNavigatePath({ path: '   ' })).toBeNull();
    expect(parseNavigatePath({ path: `/${'a'.repeat(600)}` })).toBeNull();
  });

  it('rejects a path containing control characters', () => {
    expect(parseNavigatePath({ path: '/a\nb' })).toBeNull();
  });
});

describe('hasUnsavedFormInput', () => {
  it('false when nothing is focused', () => {
    expect(hasUnsavedFormInput(document)).toBe(false);
  });

  it('false for a focused-but-empty input (mere focus is not "unsaved work")', () => {
    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();
    expect(hasUnsavedFormInput(document)).toBe(false);
  });

  it('true for a focused input/textarea holding content', () => {
    const input = document.createElement('input');
    input.value = '客戶名單整理中';
    document.body.appendChild(input);
    input.focus();
    expect(hasUnsavedFormInput(document)).toBe(true);

    document.body.innerHTML = '';
    const textarea = document.createElement('textarea');
    textarea.value = 'draft notes';
    document.body.appendChild(textarea);
    textarea.focus();
    expect(hasUnsavedFormInput(document)).toBe(true);
  });

  it('whitespace-only content does not count as unsaved work', () => {
    const input = document.createElement('input');
    input.value = '   ';
    document.body.appendChild(input);
    input.focus();
    expect(hasUnsavedFormInput(document)).toBe(false);
  });

  it('true for a non-empty contenteditable element', () => {
    const div = document.createElement('div');
    // `setAttribute`, not the `.contentEditable` property setter: jsdom does
    // not reflect the property back onto the attribute (a jsdom fidelity
    // gap, not a real-browser behavior — real markup is `contenteditable`
    // as an attribute either way).
    div.setAttribute('contenteditable', 'true');
    // jsdom only treats an element as focusable if it has an explicit
    // tabIndex — real browsers make `contenteditable` focusable implicitly,
    // but the fixture needs the nudge for `.focus()` to actually move
    // `document.activeElement`.
    div.tabIndex = 0;
    div.textContent = 'mid sentence';
    document.body.appendChild(div);
    div.focus();
    expect(hasUnsavedFormInput(document)).toBe(true);
  });

  it('false for a focused non-form element (e.g. a button)', () => {
    const button = document.createElement('button');
    document.body.appendChild(button);
    button.focus();
    expect(hasUnsavedFormInput(document)).toBe(false);
  });
});

describe('handleDashboardNavigate', () => {
  it('navigates immediately when the form is not mid-edit', () => {
    const navigate = vi.fn();
    const toastInfo = vi.fn();
    handleDashboardNavigate({ path: '/inbox?item=ap-1' }, navigate, toastInfo, 1000);
    expect(navigate).toHaveBeenCalledWith('/inbox?item=ap-1');
    expect(toastInfo).not.toHaveBeenCalled();
  });

  it('ignores an invalid payload without touching the cooldown clock', () => {
    const navigate = vi.fn();
    const toastInfo = vi.fn();
    handleDashboardNavigate({ path: '//evil.com' }, navigate, toastInfo, 1000);
    expect(navigate).not.toHaveBeenCalled();
    // A subsequent VALID event right after must still fire — the invalid
    // one must not have consumed the cooldown window.
    handleDashboardNavigate({ path: '/inbox' }, navigate, toastInfo, 1001);
    expect(navigate).toHaveBeenCalledWith('/inbox');
  });

  it('drops a repeat event inside the cooldown window', () => {
    const navigate = vi.fn();
    const toastInfo = vi.fn();
    handleDashboardNavigate({ path: '/inbox?item=a' }, navigate, toastInfo, 1000);
    handleDashboardNavigate({ path: '/inbox?item=b' }, navigate, toastInfo, 1000 + NAVIGATE_COOLDOWN_MS - 1);
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate).toHaveBeenCalledWith('/inbox?item=a');
  });

  it('allows a new event once the cooldown window has fully elapsed', () => {
    const navigate = vi.fn();
    const toastInfo = vi.fn();
    handleDashboardNavigate({ path: '/inbox?item=a' }, navigate, toastInfo, 1000);
    handleDashboardNavigate({ path: '/inbox?item=b' }, navigate, toastInfo, 1000 + NAVIGATE_COOLDOWN_MS);
    expect(navigate).toHaveBeenCalledTimes(2);
    expect(navigate).toHaveBeenLastCalledWith('/inbox?item=b');
  });

  it('shows a clickable toast instead of navigating when a form is mid-edit', () => {
    const input = document.createElement('input');
    input.value = 'still typing';
    document.body.appendChild(input);
    input.focus();

    const navigate = vi.fn();
    const toastInfo = vi.fn();
    handleDashboardNavigate({ path: '/inbox?item=ap-1' }, navigate, toastInfo, 1000);

    expect(navigate).not.toHaveBeenCalled();
    expect(toastInfo).toHaveBeenCalledTimes(1);
    const [message, action] = toastInfo.mock.calls[0];
    expect(typeof message).toBe('string');
    expect(action.label).toBeTruthy();

    // The toast's action performs the navigation the user deferred.
    action.onClick();
    expect(navigate).toHaveBeenCalledWith('/inbox?item=ap-1');
  });
});
