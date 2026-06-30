import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { ShieldCheck, Brain, MessagesSquare, Sparkles } from 'lucide-react';
import type { ComponentType } from 'react';
import { Button } from '@/components/ui';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useUiModeStore } from '@/stores/ui-mode-store';
import { isVisible, type Gated } from '@/lib/nav-visibility';
import { cn } from '@/lib/utils';

interface ValueCard extends Gated {
  readonly id: string;
  readonly icon: ComponentType<{ className?: string }>;
  readonly accent: string;
  readonly to?: string;
}

/**
 * Claw value props (TODO-genspark-workspace-shell §P4.1). Deliberately mirrors
 * Genspark Claw's four cards, but the privacy card is reframed around DuDuClaw's
 * real differentiator: it runs on *your own machine*, not a hosted cloud
 * sandbox — data never leaves. Order matters: privacy leads.
 */
const VALUE_CARDS: ValueCard[] = [
  { id: 'privacy', icon: ShieldCheck, accent: 'text-emerald-500 bg-emerald-500/10' },
  { id: 'memory', icon: Brain, accent: 'text-violet-500 bg-violet-500/10', to: '/memory' },
  { id: 'channels', icon: MessagesSquare, accent: 'text-sky-500 bg-sky-500/10', to: '/channels', minRole: 'admin' },
  { id: 'superpowers', icon: Sparkles, accent: 'text-amber-500 bg-amber-500/10', to: '/skills' },
];

/**
 * The "Claw — your first AI employee" hero shown on the idle workspace
 * (TODO §P4). `onStart` focuses the prompt bar; secondary CTA jumps to agent
 * management.
 */
export function ClawHero({ onStart }: { onStart?: () => void }) {
  const intl = useIntl();
  const navigate = useNavigate();
  const setMode = useUiModeStore((s) => s.setMode);
  const role = useAuthStore((s) => s.user?.role);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile === 'personal');

  const go = (to: string) => {
    setMode('dashboard');
    navigate(to);
  };

  return (
    <section aria-labelledby="claw-hero-title" className="space-y-5">
      <div className="text-center">
        <h2
          id="claw-hero-title"
          className="text-xl font-semibold tracking-tight text-stone-900 dark:text-stone-50"
        >
          {intl.formatMessage({ id: 'claw.title', defaultMessage: 'Claw — 您的第一位 AI 員工' })}
        </h2>
        <p className="mt-1 text-sm text-stone-500 dark:text-stone-400">
          {intl.formatMessage({
            id: 'claw.subtitle',
            defaultMessage: '跑在您自己的機器上,資料從不離開。',
          })}
        </p>
      </div>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {VALUE_CARDS.map((card) => {
          const Icon = card.icon;
          const linkable = card.to && isVisible(card, role, isPersonal);
          const Wrapper = linkable ? 'button' : 'div';
          return (
            <Wrapper
              key={card.id}
              {...(linkable
                ? {
                    type: 'button' as const,
                    onClick: () => go(card.to as string),
                    'aria-label': intl.formatMessage({ id: `claw.${card.id}.title` }),
                  }
                : {})}
              className={cn(
                'panel flex flex-col gap-2 rounded-xl p-4 text-left',
                linkable &&
                  'panel-hover focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/40'
              )}
            >
              <span className={cn('grid h-9 w-9 place-items-center rounded-lg', card.accent)}>
                <Icon className="h-5 w-5" />
              </span>
              <p className="text-sm font-semibold text-stone-800 dark:text-stone-100">
                {intl.formatMessage({ id: `claw.${card.id}.title` })}
              </p>
              <p className="text-xs leading-snug text-stone-500 dark:text-stone-400">
                {intl.formatMessage({ id: `claw.${card.id}.desc` })}
              </p>
            </Wrapper>
          );
        })}
      </div>

      <div className="flex flex-wrap items-center justify-center gap-3">
        <Button variant="primary" onClick={onStart}>
          {intl.formatMessage({ id: 'claw.start', defaultMessage: '立即開始' })}
        </Button>
        <Button variant="secondary" onClick={() => go('/agents')}>
          {intl.formatMessage({ id: 'claw.manage', defaultMessage: '管理 AI 員工' })}
        </Button>
      </div>
    </section>
  );
}
