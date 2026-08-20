import { useMemo } from 'react';
import qrcode from 'qrcode-generator';

/**
 * Pure-frontend QR renderer (no external CDN/service): encodes `value` with
 * the zero-dependency `qrcode-generator` and renders the resulting SVG.
 *
 * Extracted from `ChannelsPage.tsx` (originally built for the Telegram bind
 * link / LINE add-friend QR) so other onboarding surfaces — e.g. the
 * "訂閱帳號" setup wizard's authorize link — can reuse the exact same
 * renderer instead of a second copy.
 */
export function QrCode({ value, size = 200 }: { value: string; size?: number }) {
  const svg = useMemo(() => {
    if (!value) return '';
    // typeNumber 0 = auto-size; 'M' error correction tolerates ~15% damage.
    const qr = qrcode(0, 'M');
    qr.addData(value);
    qr.make();
    // scalable SVG so it renders crisp at any size; callers only ever pass a
    // URL they generated/control, so the inlined markup is safe.
    return qr.createSvgTag({ scalable: true });
  }, [value]);
  return (
    <div
      className="rounded-lg bg-white p-3"
      style={{ width: size, height: size }}
      aria-hidden
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}
