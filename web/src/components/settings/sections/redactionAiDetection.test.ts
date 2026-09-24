import { describe, it, expect } from 'vitest';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import type { RedactionModelStatus } from '@/lib/api';
import {
  AI_DETECT_CATEGORIES,
  deriveAiDetectView,
  downloadPercent,
  formatModelBytes,
  formatProgressCaption,
  needsNotReadyWarning,
  shortRevision,
  shouldPollModelStatus,
} from './redactionAiDetection';

function status(overrides: Partial<RedactionModelStatus>): RedactionModelStatus {
  return {
    installed: false,
    model_revision: null,
    ort_version: null,
    size_bytes: 961_500_000,
    state: 'absent',
    progress: null,
    error: null,
    avg_latency_ms: null,
    p50_latency_ms: null,
    calls: 0,
    last_used_at: null,
    ...overrides,
  };
}

// ── byte / progress formatting ──────────────────────────────────────────

describe('formatModelBytes', () => {
  it('renders whole MB below 1024MB', () => {
    expect(formatModelBytes(348_000_000)).toBe('332 MB');
  });
  it('renders one-decimal GB at and above 1024MB', () => {
    expect(formatModelBytes(1_288_490_189)).toBe('1.2 GB');
  });
  it('floors non-positive/invalid input to "0 MB" instead of "-1 MB" or "NaN MB"', () => {
    expect(formatModelBytes(0)).toBe('0 MB');
    expect(formatModelBytes(-5)).toBe('0 MB');
    expect(formatModelBytes(Number.NaN)).toBe('0 MB');
  });
  it('never rounds a tiny positive value down to 0 MB', () => {
    expect(formatModelBytes(1)).toBe('1 MB');
  });
});

describe('downloadPercent', () => {
  it('computes a clamped 0-100 integer', () => {
    expect(downloadPercent(348_000_000, 917_000_000)).toBe(38);
    expect(downloadPercent(0, 917_000_000)).toBe(0);
    expect(downloadPercent(917_000_000, 917_000_000)).toBe(100);
  });
  it('clamps a done value larger than total to 100 rather than overflowing', () => {
    expect(downloadPercent(1_000, 500)).toBe(100);
  });
  it('returns null when total is unknown (0, negative, or non-finite)', () => {
    expect(downloadPercent(100, 0)).toBeNull();
    expect(downloadPercent(100, -1)).toBeNull();
    expect(downloadPercent(100, Number.NaN)).toBeNull();
  });
});

describe('formatProgressCaption', () => {
  it('drops the first number\'s unit when both sides share one (canvas: "348 / 917 MB")', () => {
    expect(formatProgressCaption(348_000_000, 917_000_000)).toBe('332 / 875 MB');
  });
  it('keeps both units when they differ, so neither number reads as wrong', () => {
    expect(formatProgressCaption(900_000_000, 1_288_490_189)).toBe('858 MB / 1.2 GB');
  });
  it('falls back to just the done side when total is unknown', () => {
    expect(formatProgressCaption(50_000_000, 0)).toBe('48 MB');
  });
});

describe('shortRevision', () => {
  it('truncates a long revision to the given length', () => {
    expect(shortRevision('a1b2c3d4e5f6a1b2c3d4e5f6', 8)).toBe('a1b2c3d4');
  });
  it('returns a short revision unchanged', () => {
    expect(shortRevision('a1b2c3', 8)).toBe('a1b2c3');
  });
  it('returns null for null/undefined/empty input', () => {
    expect(shortRevision(null)).toBeNull();
    expect(shortRevision(undefined)).toBeNull();
    expect(shortRevision('')).toBeNull();
  });
});

// ── status → view ─────────────────────────────────────────────────────

describe('deriveAiDetectView', () => {
  it('reports "unknown" for a null status (load in flight / failed)', () => {
    expect(deriveAiDetectView(null)).toEqual({ kind: 'unknown' });
  });

  it('carries size_bytes through for the "absent" chip', () => {
    expect(deriveAiDetectView(status({ state: 'absent', size_bytes: 917_000_000 }))).toEqual({
      kind: 'absent',
      sizeBytes: 917_000_000,
    });
  });

  it('reads doneBytes/totalBytes/pct/file from progress while downloading', () => {
    expect(
      deriveAiDetectView(
        status({
          state: 'downloading',
          progress: { done_bytes: 100, total_bytes: 200, file: 'onnx/model_q4.onnx' },
        }),
      ),
    ).toEqual({ kind: 'downloading', doneBytes: 100, totalBytes: 200, pct: 50, file: 'onnx/model_q4.onnx' });
  });

  it('falls back to size_bytes/0 when downloading with no progress yet', () => {
    expect(deriveAiDetectView(status({ state: 'downloading', progress: null, size_bytes: 500 }))).toEqual({
      kind: 'downloading',
      doneBytes: 0,
      totalBytes: 500,
      pct: 0,
      file: null,
    });
  });

  it('treats "ready" and "loaded" identically', () => {
    const ready = deriveAiDetectView(
      status({ state: 'ready', model_revision: 'deadbeef1234', calls: 0, avg_latency_ms: null }),
    );
    const loaded = deriveAiDetectView(
      status({ state: 'loaded', model_revision: 'deadbeef1234', calls: 0, avg_latency_ms: null }),
    );
    expect(ready).toEqual({ kind: 'ready', revision: 'deadbeef', hasUsage: false, avgLatencyMs: null });
    expect(loaded).toEqual(ready);
  });

  it('reports hasUsage only when calls > 0', () => {
    const view = deriveAiDetectView(status({ state: 'ready', calls: 12, avg_latency_ms: 41.3 }));
    expect(view).toMatchObject({ kind: 'ready', hasUsage: true, avgLatencyMs: 41.3 });
  });

  it('carries the error message through for "error"', () => {
    expect(deriveAiDetectView(status({ state: 'error', error: 'sha256 mismatch' }))).toEqual({
      kind: 'error',
      message: 'sha256 mismatch',
    });
  });

  it('degrades an unrecognised future state to "unknown" instead of throwing', () => {
    // @ts-expect-error — deliberately testing the defensive fallback against a value outside the known union.
    expect(deriveAiDetectView(status({ state: 'quantum_uncertain' }))).toEqual({ kind: 'unknown' });
  });
});

describe('shouldPollModelStatus', () => {
  it('polls only while downloading', () => {
    expect(shouldPollModelStatus({ kind: 'downloading', doneBytes: 1, totalBytes: 2, pct: 50, file: null })).toBe(true);
    expect(shouldPollModelStatus({ kind: 'absent', sizeBytes: 1 })).toBe(false);
    expect(shouldPollModelStatus({ kind: 'ready', revision: null, hasUsage: false, avgLatencyMs: null })).toBe(false);
    expect(shouldPollModelStatus({ kind: 'error', message: null })).toBe(false);
    expect(shouldPollModelStatus({ kind: 'unknown' })).toBe(false);
  });
});

describe('needsNotReadyWarning', () => {
  it('warns when checked and not ready (absent/downloading/error)', () => {
    expect(needsNotReadyWarning({ kind: 'absent', sizeBytes: 1 }, true)).toBe(true);
    expect(
      needsNotReadyWarning({ kind: 'downloading', doneBytes: 1, totalBytes: 2, pct: 50, file: null }, true),
    ).toBe(true);
    expect(needsNotReadyWarning({ kind: 'error', message: null }, true)).toBe(true);
  });
  it('never warns when unchecked, regardless of state', () => {
    expect(needsNotReadyWarning({ kind: 'absent', sizeBytes: 1 }, false)).toBe(false);
  });
  it('never warns once ready — that is the whole point of the warning', () => {
    expect(needsNotReadyWarning({ kind: 'ready', revision: null, hasUsage: false, avgLatencyMs: null }, true)).toBe(
      false,
    );
  });
  it('never warns on "unknown" — a load failure says nothing about install state', () => {
    expect(needsNotReadyWarning({ kind: 'unknown' }, true)).toBe(false);
  });
});

// ── category list ────────────────────────────────────────────────────

describe('AI_DETECT_CATEGORIES', () => {
  it('is exactly the 8 categories from §5, in canvas chip order', () => {
    expect(AI_DETECT_CATEGORIES).toEqual([
      'PERSON',
      'ADDRESS',
      'EMAIL',
      'PHONE',
      'URL',
      'DATE',
      'ACCOUNT_NUMBER',
      'SECRET',
    ]);
  });
});

// ── i18n parity (mirrors `memory-freshness.test.ts`'s convention) ──────

describe('AI 智慧偵測 i18n coverage', () => {
  const catalogues = { en, 'zh-TW': zhTW, 'ja-JP': jaJP } as Record<string, Record<string, string>>;

  it('ships every redaction.aiDetect.* string and the 8 category labels in all three catalogues', () => {
    const required = [
      'redaction.profile.ai_pii',
      'redaction.aiDetect.chip.localModel',
      'redaction.aiDetect.desc',
      'redaction.aiDetect.status.unknown',
      'redaction.aiDetect.status.absent',
      'redaction.aiDetect.status.absent.button',
      'redaction.aiDetect.status.downloading',
      'redaction.aiDetect.status.cancel',
      'redaction.aiDetect.status.ready',
      'redaction.aiDetect.status.ready.noRevision',
      'redaction.aiDetect.status.avgLatency',
      'redaction.aiDetect.status.notUsedYet',
      'redaction.aiDetect.status.error',
      'redaction.aiDetect.status.retry',
      'redaction.aiDetect.install.started',
      'redaction.aiDetect.install.error',
      'redaction.aiDetect.cancel.error',
      'redaction.aiDetect.warnNotReady',
      'redaction.aiDetect.alert.title',
      'redaction.aiDetect.alert.body',
      ...AI_DETECT_CATEGORIES.map((cat) => `redaction.cat.${cat}`),
    ];
    for (const [locale, catalogue] of Object.entries(catalogues)) {
      const missing = required.filter((key) => !catalogue[key]?.trim());
      expect(missing, `${locale} is missing keys`).toEqual([]);
    }
  });
});
