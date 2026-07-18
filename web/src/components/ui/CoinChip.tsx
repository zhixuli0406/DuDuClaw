import { Coins } from 'lucide-react';
import { cn } from '@/lib/utils';
import { formatCoins } from '@/lib/format';

/**
 * CoinChip — the Header spend/coin capsule (§4.3). Shows a money amount stored
 * as integer cents in the `--coin` amber-gold hue. Clickable to route to
 * billing. Machine value is tabular for calm ticking.
 */
export function CoinChip({
  cents,
  currency = 'USD',
  onClick,
  title,
  className,
}: {
  cents: number | null | undefined;
  currency?: 'USD' | 'TWD';
  onClick?: () => void;
  title?: string;
  className?: string;
}) {
  const content = (
    <>
      <Coins className="h-4 w-4 shrink-0 text-[color:var(--coin)]" aria-hidden="true" />
      <span className="font-mono text-xs font-medium tabular-nums text-foreground">
        {formatCoins(cents, currency)}
      </span>
    </>
  );
  const cls = cn(
    'inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 ring-1 ring-inset ring-[color:var(--coin)]/25 bg-[color:var(--coin)]/10',
    className,
  );
  if (onClick) {
    return (
      <button
        type="button"
        onClick={onClick}
        title={title}
        className={cn(cls, 'outline-none hover:bg-[color:var(--coin)]/15 focus-visible:ring-3 focus-visible:ring-ring/50')}
      >
        {content}
      </button>
    );
  }
  return (
    <span className={cls} title={title}>
      {content}
    </span>
  );
}
