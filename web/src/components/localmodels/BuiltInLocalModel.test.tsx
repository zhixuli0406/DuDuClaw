import { describe, expect, it } from 'vitest';

import { bannerState } from './BuiltInLocalModel';
import type { LocalInferenceStatus } from '@/lib/api';

/**
 * The banner is the one line a person reads to decide whether the local
 * model is usable right now, so the mapping from probe results to that line
 * is worth pinning. The ordering matters as much as the cases: a reachable
 * engine is "running" no matter what the saved settings say, and "no model
 * downloaded" must never be shown for a device that is actually answering.
 */

function status(over: Partial<LocalInferenceStatus> = {}): LocalInferenceStatus {
  return {
    appliance: true,
    llama_server_present: true,
    endpoint: 'http://127.0.0.1:8080/v1',
    reachable: false,
    loaded_model: null,
    configured_model: null,
    models_dir: '/data/duduclaw/models',
    has_model: false,
    installed: [],
    downloads: [],
    ...over,
  };
}

describe('bannerState', () => {
  it('is loading until the first probe lands', () => {
    expect(bannerState(null)).toBe('loading');
  });

  it('is unsupported only when neither the flag nor the engine is present', () => {
    expect(bannerState(status({ appliance: false, llama_server_present: false }))).toBe(
      'unsupported',
    );
    // A platform build with the engine installed by hand still gets the panel.
    expect(bannerState(status({ appliance: false, llama_server_present: true }))).not.toBe(
      'unsupported',
    );
  });

  it('reports running from the live probe, not from the saved settings', () => {
    // Reachable with nothing recorded in settings: still running.
    expect(bannerState(status({ reachable: true, loaded_model: 'Qwen3-4B.gguf' }))).toBe('running');
    // Settings name a model but the engine is down: not running.
    expect(bannerState(status({ configured_model: 'Qwen3-4B.gguf', has_model: true }))).toBe(
      'stopped',
    );
  });

  it('never says "nothing downloaded" about a device that is answering', () => {
    expect(bannerState(status({ reachable: true, has_model: false }))).toBe('running');
  });

  it('distinguishes nothing-downloaded from downloaded-but-off', () => {
    expect(bannerState(status({ has_model: false }))).toBe('nomodel');
    expect(bannerState(status({ has_model: true }))).toBe('stopped');
  });
});
