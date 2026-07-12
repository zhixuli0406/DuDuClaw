import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Info, Globe, Mail, Building2 } from 'lucide-react';
import { Page, PageHeader, Card, Mono } from '@/components/ui';
import { api, type AboutResponse, type BrandingVendor } from '@/lib/api';
import { useEffectiveName, useEffectiveLogo } from '@/lib/branding';
import { tierLabel } from '@/lib/license-labels';

/**
 * About page (design-distributor-white-label §4.3) — open to every
 * authenticated user on every instance.
 *
 * Upper half: distributor-authored brand info (logo, product name, description,
 * company, website, support email). Lower half: the FIXED upstream-vendor block
 * ("嘟嘟數位科技有限公司") which a distributor can never overwrite — it comes from
 * the backend `vendor` const, with a hard-coded front-end fallback as a second
 * safety net so the attribution shows even if the RPC fails.
 */

/** Hard-coded fallback mirroring the backend `UPSTREAM_VENDOR_*` consts. */
const VENDOR_FALLBACK: BrandingVendor = {
  name_zh: '嘟嘟數位科技有限公司',
  name_en: 'DuDu Digital Technology Co., Ltd.',
  url: 'https://duduclaw.dudustudio.monster',
};

export function AboutPage() {
  const intl = useIntl();
  const brandName = useEffectiveName();
  const brandLogo = useEffectiveLogo();
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

  const branding = about?.branding ?? null;
  const vendor = about?.vendor ?? VENDOR_FALLBACK;
  const description = branding?.description?.trim();
  const companyName = branding?.company_name?.trim();
  const website = branding?.website?.trim();
  const supportEmail = branding?.support_email?.trim();
  // Distributor-authored HTML block (design §10.2). Already sanitized by the
  // backend before it reaches us — rendered as-is, never from raw input.
  const aboutHtml = branding?.about_html?.trim();

  return (
    <Page>
      <PageHeader
        icon={Info}
        title={intl.formatMessage({ id: 'about.title' })}
        subtitle={intl.formatMessage({ id: 'about.subtitle' })}
      />

      {/* Upper — distributor brand */}
      <Card>
        <div className="flex items-start gap-4">
          {brandLogo.isImage ? (
            <img
              src={brandLogo.value}
              alt={brandName}
              className="h-16 w-16 shrink-0 rounded-2xl object-cover"
            />
          ) : (
            <span
              className="grid h-16 w-16 shrink-0 place-items-center rounded-2xl bg-gradient-to-b from-amber-400 to-amber-500 text-3xl"
              role="img"
              aria-label={brandName}
            >
              {brandLogo.value}
            </span>
          )}
          <div className="min-w-0 flex-1">
            <h2 className="text-xl font-semibold tracking-tight text-stone-900 dark:text-stone-50">
              {brandName}
            </h2>
            {!aboutHtml && description && (
              <p className="mt-1 text-sm text-stone-600 dark:text-stone-300">{description}</p>
            )}
          </div>
        </div>

        {aboutHtml && (
          // Safe: `aboutHtml` is the backend-sanitized About block, not raw input.
          <div
            className="branding-about-preview mt-5 border-t border-stone-200/70 pt-4 text-sm text-stone-700 dark:border-white/8 dark:text-stone-200"
            dangerouslySetInnerHTML={{ __html: aboutHtml }}
          />
        )}

        {!aboutHtml && (companyName || website || supportEmail) && (
          <dl className="mt-5 space-y-3 border-t border-stone-200/70 pt-4 dark:border-white/8">
            {companyName && (
              <div className="flex items-center gap-2 text-sm">
                <Building2 className="h-4 w-4 shrink-0 text-stone-400" />
                <dt className="sr-only">{intl.formatMessage({ id: 'about.company' })}</dt>
                <dd className="text-stone-700 dark:text-stone-200">{companyName}</dd>
              </div>
            )}
            {website && (
              <div className="flex items-center gap-2 text-sm">
                <Globe className="h-4 w-4 shrink-0 text-stone-400" />
                <dt className="sr-only">{intl.formatMessage({ id: 'about.website' })}</dt>
                <dd className="min-w-0 truncate">
                  <a
                    href={website}
                    target="_blank"
                    rel="noreferrer noopener"
                    className="text-amber-600 hover:text-amber-700 dark:text-amber-400"
                  >
                    {website}
                  </a>
                </dd>
              </div>
            )}
            {supportEmail && (
              <div className="flex items-center gap-2 text-sm">
                <Mail className="h-4 w-4 shrink-0 text-stone-400" />
                <dt className="sr-only">{intl.formatMessage({ id: 'about.supportEmail' })}</dt>
                <dd className="min-w-0 truncate">
                  <a
                    href={`mailto:${supportEmail}`}
                    className="text-amber-600 hover:text-amber-700 dark:text-amber-400"
                  >
                    {supportEmail}
                  </a>
                </dd>
              </div>
            )}
          </dl>
        )}
      </Card>

      {/* Lower — fixed upstream-vendor block (never overwritable) */}
      <Card>
        <p className="text-[11px] font-semibold uppercase tracking-wider text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'about.softwareBy' })}
        </p>
        <p className="mt-1.5 text-sm font-medium text-stone-800 dark:text-stone-100">
          {vendor.name_zh}
          <span className="ml-1.5 text-stone-500 dark:text-stone-400">（{vendor.name_en}）</span>
        </p>
        <div className="mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
          <a
            href={vendor.url}
            target="_blank"
            rel="noreferrer noopener"
            className="inline-flex items-center gap-1.5 text-amber-600 hover:text-amber-700 dark:text-amber-400"
          >
            <Globe className="h-4 w-4" />
            {vendor.url.replace(/^https?:\/\//, '')}
          </a>
          <span className="text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'about.version' })}{' '}
            <Mono>{about?.version ?? '—'}</Mono>
          </span>
          {about?.tier && (
            <span className="text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'about.edition' })}{' '}
              <Mono>{tierLabel(about.tier)}</Mono>
            </span>
          )}
        </div>
      </Card>
    </Page>
  );
}
