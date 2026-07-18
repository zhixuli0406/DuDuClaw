import { type HTMLAttributes } from 'react';
import { cn } from '@/lib/utils';

/** Skeleton — MDS loading placeholder (spec §4 Sidebar 系列). */
export function Skeleton({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="skeleton"
      className={cn('animate-pulse rounded-md bg-muted', className)}
      {...props}
    />
  );
}
