import type { ComponentProps } from 'react';
import { useIntl } from 'react-intl';
import { ExternalLink } from 'lucide-react';
import { Button } from '@/components/mds';
import { channelLabel } from '@/lib/session-channel';

/**
 * "在 <平台名> 中開啟" — W2-3 reverse handoff (04 doc §E8 / 02 doc §P5-3):
 * jump from a dashboard object (task, approval) back to the channel
 * conversation it came from or is currently parked in.
 *
 * Renders nothing when there is no resolved `link` — the backend
 * (`channel_link.rs`) already decided per-platform whether a URL can be
 * built at all; the frontend never shows a placeholder or a guessed link
 * for a platform it can't reach (E8 brief: "組不出來就回 None,前端不顯示按鈕").
 */
export function OpenInChannelButton({
  channel,
  link,
  size = 'sm',
  variant = 'outline',
  className,
}: {
  channel?: string | null;
  link?: string | null;
  size?: ComponentProps<typeof Button>['size'];
  variant?: ComponentProps<typeof Button>['variant'];
  className?: string;
}) {
  const intl = useIntl();
  if (!link || !channel) return null;
  return (
    <Button
      variant={variant}
      size={size}
      className={className}
      onClick={() => window.open(link, '_blank', 'noopener,noreferrer')}
    >
      <ExternalLink />
      {intl.formatMessage({ id: 'channelLink.openIn' }, { channel: channelLabel(channel) })}
    </Button>
  );
}
