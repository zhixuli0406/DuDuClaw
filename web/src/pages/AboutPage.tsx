import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Info, Globe } from 'lucide-react';
import { Card, CardContent } from '@/components/mds';
import { api, type AboutResponse, type SoftwareVendor } from '@/lib/api';
import { useEffectiveName } from '@/lib/product-identity';
import { BrandMark } from '@/components/brand/BrandMark';
import { tierLabel } from '@/lib/license-labels';
import { DistributorNoticeCard } from '@/components/about/DistributorNoticeCard';

/** Hard-coded fallback mirroring the backend `UPSTREAM_VENDOR_*` consts. */
const VENDOR_FALLBACK: SoftwareVendor = {
  name_zh: '嘟嘟數位科技有限公司',
  name_en: 'Dudu Technology Ltd.',
  url: 'https://duduclaw.dudustudio.monster',
};

export function AboutPage() {
  const intl = useIntl();
  const brandName = useEffectiveName();
  const [about, setAbout] = useState<AboutResponse | null>(null);

  useEffect(() => {
    let active = true;
    api.about
      .get()
      .then((res) => {
        if (active) setAbout(res);
      })
      .catch(() => {
        /* about is best-effort; the fixed vendor fallback still renders */
      });
    return () => {
      active = false;
    };
  }, []);

  const vendor = about?.vendor ?? VENDOR_FALLBACK;

  return (
    <div className="space-y-6">
      {/* Slim page header (spec §5.2). */}
      <div className="flex items-center gap-2">
        <Info className="size-5 text-muted-foreground" />
        <div>
          <h1 className="text-base font-medium">{intl.formatMessage({ id: 'about.title' })}</h1>
          <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'about.subtitle' })}</p>
        </div>
      </div>

      {/* Fixed software identity */}
      <Card>
        <CardContent>
          <div className="flex items-start gap-4">
            <BrandMark size={64} title={brandName} />
            <div className="min-w-0 flex-1">
              <h2 className="text-xl font-semibold tracking-tight text-foreground">{brandName}</h2>
            </div>
          </div>

        </CardContent>
      </Card>

      {/* Distributor information — only present in a distributor build; the
          HTML is a build input, sanitized server-side and sandboxed here. */}
      {typeof about?.distributor_notice === 'string' && about.distributor_notice.length > 0 && (
        <DistributorNoticeCard html={about.distributor_notice} />
      )}

      {/* Lower — fixed upstream-vendor block (never overwritable) */}
      <Card>
        <CardContent>
          <p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            {intl.formatMessage({ id: 'about.softwareBy' })}
          </p>
          <p className="mt-1.5 text-sm font-medium text-foreground">
            {vendor.name_zh}
            <span className="ml-1.5 text-muted-foreground">（{vendor.name_en}）</span>
          </p>
          <div className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
            <a
              href={vendor.url}
              target="_blank"
              rel="noreferrer noopener"
              className="inline-flex items-center gap-1.5 text-brand hover:underline"
            >
              <Globe className="h-4 w-4" />
              {vendor.url.replace(/^https?:\/\//, '')}
            </a>
            <span className="text-muted-foreground">
              {intl.formatMessage({ id: 'about.version' })}{' '}
              <code className="font-mono text-xs">{about?.version ?? '—'}</code>
            </span>
            {about?.tier && (
              <span className="text-muted-foreground">
                {intl.formatMessage({ id: 'about.edition' })}{' '}
                <code className="font-mono text-xs">{tierLabel(about.tier)}</code>
              </span>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
