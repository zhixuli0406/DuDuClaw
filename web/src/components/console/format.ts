/**
 * Tiny pure formatters local to the console artifact cards. Deliberately NOT
 * imported from `DevicePage.tsx` — this design doc's whole §3 is about
 * cutting cross-page coupling when splitting surfaces apart, so a ~10-line
 * pure formatter is duplicated here rather than reaching into a settings
 * page's module for it. Logic mirrors `DevicePage.tsx`'s `formatMb` /
 * `formatUptime` (kept in sync by inspection, not by import).
 */

/** MB → a compact "1.2 GB" / "512 MB" token. */
export function formatMb(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  return `${Math.round(mb)} MB`;
}

/** Whole-seconds uptime → "3d 4h" / "2h 15m" / "45m". */
export function formatUptime(secs: number): string {
  const days = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  const mins = Math.floor((secs % 3600) / 60);
  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  return `${mins}m`;
}

/** Bytes → a compact "1.2 GB" / "512 MB" token (backup file sizes are bytes,
 *  not MB — `device.backup_list`'s `size` field). */
export function formatBytes(bytes: number): string {
  return formatMb(bytes / (1024 * 1024));
}
