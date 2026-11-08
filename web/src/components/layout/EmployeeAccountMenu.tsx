import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { useIntl } from 'react-intl';
import { ChevronsUpDown, Languages, LogOut, Monitor, Moon, Sun } from 'lucide-react';
import { api, type PersonalGoogleStatus } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useThemeStore } from '@/stores/theme-store';
import {
  DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem,
  DropdownMenuLabel, DropdownMenuSeparator,
} from '@/components/mds';

/** Uses exactly the same connection status as /me/connections. */
export function connectionMissing(status: PersonalGoogleStatus): boolean {
  return !status.connection || status.connection.status !== 'connected'
    || status.requires_login || !status.integration_enabled
    || status.requests.some(request => request.allowed !== true);
}

export function EmployeeAccountMenu({ collapsed }: { collapsed: boolean }) {
  const intl = useIntl();
  const navigate = useNavigate();
  const user = useAuthStore(s => s.user);
  const logout = useAuthStore(s => s.logout);
  const theme = useThemeStore(s => s.theme);
  const cycleTheme = useThemeStore(s => s.cycleTheme);
  const [distributorHelp, setDistributorHelp] = useState(false);
  const [missing, setMissing] = useState(false);
  const mounted = useRef(false);
  const mountRequest = useRef<Promise<PersonalGoogleStatus> | null>(null);
  const requestId = useRef(0);
  const open = useRef(false);
  const applyRequest = useCallback((request: Promise<PersonalGoogleStatus>) => {
    const id = ++requestId.current;
    void request.then(status => {
      if (mounted.current && id === requestId.current) setMissing(connectionMissing(status));
    }).catch(() => {
      if (mounted.current && id === requestId.current) setMissing(false);
    });
  }, []);
  useEffect(() => {
    mounted.current = true;
    void api.about.get().then(about => {
      if (mounted.current) setDistributorHelp(Boolean(about.distributor_notice?.trim()));
    }).catch(() => {});
    mountRequest.current ??= api.personalGoogle.status();
    applyRequest(mountRequest.current);
    return () => { mounted.current = false; };
  }, [applyRequest]);
  const ThemeIcon = theme === 'light' ? Sun : theme === 'dark' ? Moon : Monitor;
  const t = (id: string) => intl.formatMessage({ id });
  return (
    <DropdownMenu onOpenChange={next => {
      if (next && !open.current) applyRequest(api.personalGoogle.status());
      open.current = next;
    }}>
      <DropdownMenuTrigger className="flex w-full items-center gap-2 rounded-md p-2 text-left outline-none hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring" aria-label={user?.display_name}>
        {!collapsed && <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium">{user?.display_name}</span>
          <span className="block truncate text-xs text-muted-foreground">{user?.department}</span>
        </span>}
        <ChevronsUpDown className="size-4 shrink-0" />
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-56">
        <DropdownMenuLabel className="max-w-72 truncate">{user?.email}</DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => navigate('/me/connections')}>
          {t('myConnections.title')}
          {missing && <span role="img" aria-label={t('sidebar.connectionsMissing')} className="size-1.5 rounded-full bg-warning" />}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => navigate('/me/memory')}>{t('nav.myMemory')}</DropdownMenuItem>
        <DropdownMenuItem onClick={() => navigate('/me/account')}>{t('sidebar.employeeSettings')}</DropdownMenuItem>
        <DropdownMenuItem onClick={cycleTheme} aria-label={intl.formatMessage({ id: 'header.theme' }, { theme })}>
          <ThemeIcon />{t('sidebar.employeeTheme')}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => navigate('/me/account#language')}>
          <Languages />{t('header.language')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => distributorHelp
          ? navigate('/about#distributor-support')
          : window.open('https://github.com/zhixuli0406/DuDuClaw/tree/main/docs/guides', '_blank', 'noopener,noreferrer')}>
          {t('sidebar.employeeHelp')}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onClick={() => logout()}><LogOut />{t('auth.logout')}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
