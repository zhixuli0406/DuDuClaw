import type { KbEmbeddingProvider, KbEmbeddingSettings } from '@/lib/knowledge-embedding-api';

/** Poll while a download runs or vectors are still being built. */
export const EMBEDDING_POLL_MS = 2000;

export function shouldPoll(s: KbEmbeddingSettings | null): boolean {
  if (!s) return false;
  if (s.job.state === 'downloading') return true;
  return s.available && s.chunks_total > 0 && s.chunks_with_vectors < s.chunks_total;
}

/** `1234567` → `1.2 MB`. */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0 MB';
  const mb = n / (1024 * 1024);
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}

/** Whole percent, clamped. */
export function percent(fraction: number): number {
  if (!Number.isFinite(fraction)) return 0;
  return Math.max(0, Math.min(100, Math.round(fraction * 100)));
}

export function isCloud(p: KbEmbeddingProvider): p is 'openai' | 'gemini' {
  return p === 'openai' || p === 'gemini';
}

/** What a cloud choice will send to, for the disclosure dialog. */
export function disclosureTarget(
  s: KbEmbeddingSettings,
  provider: 'openai' | 'gemini',
  model: string,
): { vendor: string; host: string; model: string } {
  const p = s.cloud_providers.find((c) => c.id === provider);
  // The configured host wins when the configured provider is the one chosen
  // (an operator may point at a regional endpoint in config.toml).
  const host = s.cloud?.provider === provider && s.cloud.host ? s.cloud.host : (p?.host ?? '');
  return { vendor: p?.vendor ?? provider, host, model: model.trim() || p?.default_model || '' };
}
