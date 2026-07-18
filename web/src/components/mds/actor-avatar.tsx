import { forwardRef, useState, type ComponentPropsWithoutRef } from 'react';
import { cva, type VariantProps } from 'class-variance-authority';
import { BotIcon, SparklesIcon, UserIcon, UsersIcon } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * ActorAvatar — MDS unified avatar (spec §4 ActorAvatar). `actorType` picks the
 * fallback glyph; a plain circle with `ring-1` (no gradient halo) is the Multica
 * house style. `showStatusDot` overlays an availability dot at the bottom-right.
 * Every place an agent name appears should carry one of these.
 */

export type ActorType = 'user' | 'agent' | 'system' | 'squad';
export type ActorStatus = 'online' | 'busy' | 'offline' | 'error';
export type ActorAvatarSize = 'xs' | 'sm' | 'md' | 'lg' | 'xl' | '2xl';

const FALLBACK_ICON: Record<
  ActorType,
  React.ComponentType<{ className?: string }>
> = {
  user: UserIcon,
  agent: BotIcon,
  system: SparklesIcon,
  squad: UsersIcon,
};

const STATUS_COLOR: Record<ActorStatus, string> = {
  online: 'bg-success',
  busy: 'bg-warning',
  offline: 'bg-muted-foreground',
  error: 'bg-destructive',
};

const avatarVariants = cva(
  'relative inline-flex shrink-0 items-center justify-center overflow-hidden rounded-full bg-muted text-muted-foreground ring-1 ring-surface-border',
  {
    variants: {
      size: {
        xs: 'size-4 [&_svg]:size-2.5',
        sm: 'size-5 [&_svg]:size-3',
        md: 'size-6 [&_svg]:size-3.5',
        lg: 'size-8 [&_svg]:size-4',
        xl: 'size-10 [&_svg]:size-5',
        '2xl': 'size-14 [&_svg]:size-7',
      },
    },
    defaultVariants: { size: 'md' },
  }
);

const DOT_POSITION: Record<ActorAvatarSize, string> = {
  xs: '-right-0 -bottom-0',
  sm: '-right-0 -bottom-0',
  md: 'right-0 bottom-0',
  lg: 'right-0 bottom-0',
  xl: 'right-0.5 bottom-0.5',
  '2xl': 'right-1 bottom-1',
};

export const ActorAvatar = forwardRef<
  HTMLSpanElement,
  Omit<ComponentPropsWithoutRef<'span'>, 'children'> &
    VariantProps<typeof avatarVariants> & {
      actorType?: ActorType;
      src?: string;
      alt?: string;
      name?: string;
      showStatusDot?: boolean;
      status?: ActorStatus;
    }
>(
  (
    {
      className,
      size = 'md',
      actorType = 'user',
      src,
      alt,
      name,
      showStatusDot = false,
      status = 'offline',
      ...props
    },
    ref
  ) => {
    const [imgFailed, setImgFailed] = useState(false);
    const Icon = FALLBACK_ICON[actorType];
    const showImage = src && !imgFailed;

    return (
      <span
        ref={ref}
        data-slot="actor-avatar"
        data-actor-type={actorType}
        className={cn(avatarVariants({ size }), className)}
        {...props}
      >
        {showImage ? (
          <img
            src={src}
            alt={alt ?? name ?? actorType}
            className="size-full object-cover"
            onError={() => setImgFailed(true)}
          />
        ) : (
          <Icon aria-hidden data-slot="actor-avatar-fallback" />
        )}
        {showStatusDot && (
          <span
            data-slot="actor-avatar-status"
            data-status={status}
            aria-label={status}
            className={cn(
              'absolute size-1.5 rounded-full ring-1 ring-surface',
              STATUS_COLOR[status],
              DOT_POSITION[(size ?? 'md') as ActorAvatarSize]
            )}
          />
        )}
      </span>
    );
  }
);
ActorAvatar.displayName = 'ActorAvatar';
