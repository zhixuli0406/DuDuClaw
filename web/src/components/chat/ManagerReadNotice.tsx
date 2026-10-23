import { useIntl } from 'react-intl';
import { Eye } from 'lucide-react';

export function ManagerReadNotice({ visible }: { visible: boolean }) {
  const intl = useIntl();
  if (!visible) return null;
  const lang = intl.locale === 'zh-TW' ? 'zh-TW/' : intl.locale === 'ja-JP' ? 'ja-JP/' : '';
  return <p className="mt-2 flex items-start gap-1.5 text-xs text-muted-foreground">
    <Eye className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
    <a className="underline underline-offset-2" href={`https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/features/${lang}68-manager-team-history.md`} target="_blank" rel="noopener noreferrer">{intl.formatMessage({ id: 'teamHistory.notice' })}</a>
  </p>;
}
