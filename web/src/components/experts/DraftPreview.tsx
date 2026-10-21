import { useIntl } from 'react-intl';
import { AlertTriangle, BookOpen, Globe, Puzzle, ShieldAlert, Users } from 'lucide-react';
import { Badge } from '@/components/mds';
import type { ExpertDraftAgent, ExpertDraftPreview, ExpertInstallWarning } from '@/lib/api';

function MemberCard({ agent }: { agent: ExpertDraftAgent }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);
  const skills = agent.skills ?? [];
  const redLines = agent.red_lines ?? [];
  const mustAlways = agent.must_always ?? [];
  return (
    <li className="rounded-lg border border-surface-border p-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm font-medium">{agent.display_name || agent.name}</span>
        <Badge variant={agent.role === 'front_desk' ? 'secondary' : 'ghost'}>
          {agent.role === 'front_desk'
            ? t('experts.generate.roleFrontDesk')
            : t('experts.generate.roleWorker')}
        </Badge>
        {agent.placeholder && (
          <Badge variant="destructive">{t('experts.generate.placeholder')}</Badge>
        )}
      </div>
      {agent.soul_excerpt && (
        <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">{agent.soul_excerpt}</p>
      )}
      <div className="mt-2 flex flex-wrap items-center gap-1.5 text-xs">
        {skills.length === 0 ? (
          <span className="text-muted-foreground">{t('experts.generate.noSkills')}</span>
        ) : (
          skills.map((s) => (
            <Badge key={s.name} variant="secondary" title={s.description || undefined}>
              <Puzzle />
              {s.description || s.name}
            </Badge>
          ))
        )}
      </div>
      {(redLines.length > 0 || mustAlways.length > 0) && (
        <details className="mt-2 text-xs">
          <summary className="cursor-pointer text-muted-foreground">
            <ShieldAlert className="mr-1 inline size-3.5" aria-hidden />
            {t('experts.generate.redLines', { count: redLines.length })}
          </summary>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {redLines.map((r) => (
              <li key={`n-${r}`}>{r}</li>
            ))}
          </ul>
          {mustAlways.length > 0 && (
            <>
              <p className="mt-1.5 text-muted-foreground">{t('experts.generate.mustAlways')}</p>
              <ul className="mt-0.5 list-disc space-y-0.5 pl-5">
                {mustAlways.map((r) => (
                  <li key={`a-${r}`}>{r}</li>
                ))}
              </ul>
            </>
          )}
        </details>
      )}
    </li>
  );
}

/** Human description of one predicted install skip (no internal paths).
 *  Unknown kind/reason pairs fall back to a generic line instead of
 *  showing a raw message id. */
export function warningText(
  w: ExpertInstallWarning,
  preview: ExpertDraftPreview,
  t: (id: string, values?: Record<string, string | number>) => string,
  has: (id: string) => boolean,
): string {
  const agentLabel =
    preview.agents.find((a) => a.name === w.name)?.display_name || w.name;
  const pageLabel = preview.wiki_pages?.find((p) => p.file === w.name)?.title || w.name;
  const name =
    w.kind === 'wiki' ? pageLabel : w.kind === 'agent' || w.kind === 'contract' ? agentLabel : w.name;
  const id = `experts.generate.warn.${w.kind}.${w.reason}`;
  return has(id) ? t(id, { name }) : t('experts.generate.warn.generic', { name });
}

/** The draft preview: summary, members (skills + red lines), knowledge base,
 *  partial-generation notes and predicted install skips. */
export function DraftPreview({ preview }: { preview: ExpertDraftPreview }) {
  const intl = useIntl();
  const has = (id: string) => Object.prototype.hasOwnProperty.call(intl.messages, id);
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);
  const pages: NonNullable<ExpertDraftPreview['wiki_pages']> =
    preview.wiki_pages ?? preview.wiki_titles.map((title) => ({ file: title, title, chars: 0 }));
  const notes = preview.notes ?? [];
  const warnings = preview.install_warnings ?? [];
  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-surface-border bg-muted/40 p-3">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-medium">{preview.display_name || preview.slug}</h3>
        </div>
        {preview.description && (
          <p className="mt-1 text-xs text-muted-foreground">{preview.description}</p>
        )}
        <div className="mt-2 flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
          <span className="inline-flex items-center gap-1">
            <Users className="size-3.5" />
            {t('experts.card.agents', { count: preview.agents.length })}
          </span>
          <span className="inline-flex items-center gap-1">
            <Puzzle className="size-3.5" />
            {t('experts.card.skills', { count: preview.skills.length })}
          </span>
          <span className="inline-flex items-center gap-1">
            <BookOpen className="size-3.5" />
            {t('experts.card.wiki', { count: pages.length })}
          </span>
        </div>
      </div>

      {notes.length > 0 && (
        <div className="rounded-lg border border-warning/30 bg-warning/10 p-3 text-xs">
          <p className="flex items-center gap-1.5 font-medium text-warning">
            <AlertTriangle className="size-3.5" aria-hidden />
            {t('experts.generate.partialTitle')}
          </p>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {notes.map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        </div>
      )}

      <div className="space-y-2">
        <span className="text-sm font-medium">{t('experts.generate.roster')}</span>
        <p className="text-xs text-muted-foreground">{t('experts.generate.redLinesHint')}</p>
        <ul className="space-y-2">
          {preview.agents.map((a) => (
            <MemberCard key={a.name} agent={a} />
          ))}
        </ul>
      </div>

      <div className="space-y-1.5">
        <span className="text-sm font-medium">{t('experts.generate.wikiTitle')}</span>
        {pages.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('experts.generate.noWiki')}</p>
        ) : (
          <>
            <ul className="flex flex-wrap gap-1.5 text-xs">
              {pages
                .filter((p) => !p.web_research)
                .map((p) => (
                  <li key={p.file}>
                    <Badge variant="ghost">
                      <BookOpen />
                      {p.title}
                    </Badge>
                  </li>
                ))}
            </ul>
            {pages.some((p) => p.web_research) && (
              <div className="space-y-1.5 rounded-lg border border-warning/30 bg-warning/10 p-3 text-xs">
                <p className="font-medium text-warning">{t('experts.generate.webPagesTitle')}</p>
                {pages
                  .filter((p) => p.web_research)
                  .map((p) => (
                    <details key={p.file}>
                      <summary className="cursor-pointer">
                        <Globe className="mr-1 inline size-3.5" aria-hidden />
                        {p.title}
                      </summary>
                      <pre className="mt-1 max-h-64 overflow-y-auto whitespace-pre-wrap rounded bg-background p-2 font-sans">
                        {p.content ?? ''}
                      </pre>
                    </details>
                  ))}
              </div>
            )}
          </>
        )}
      </div>

      {warnings.length > 0 && (
        <div className="rounded-lg border border-warning/30 bg-warning/10 p-3 text-xs">
          <p className="font-medium text-warning">{t('experts.generate.warnTitle')}</p>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {warnings.map((w) => (
              <li key={`${w.kind}-${w.name}-${w.reason}`}>{warningText(w, preview, t, has)}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
