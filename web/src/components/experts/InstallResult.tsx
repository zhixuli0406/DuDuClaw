import { useIntl } from 'react-intl';
import { AlertTriangle, Check } from 'lucide-react';
import type { ExpertDraftPreview, ExpertInstallPlacement } from '@/lib/api';

/** Lines describing everything the install did NOT place. */
export function placementProblems(
  placement: ExpertInstallPlacement,
  preview: ExpertDraftPreview | null,
  t: (id: string, values?: Record<string, string | number>) => string,
): string[] {
  const lines: string[] = [];
  for (const a of placement.agents) {
    const name = a.display_name || a.name;
    if (a.installed && a.renamed) {
      const wanted = preview?.agents.find((p) => p.name === a.name)?.display_name || a.name;
      lines.push(t('experts.install.result.renamed', { from: wanted, to: name }));
    }
    if (!a.installed) {
      lines.push(t('experts.install.result.agentMissing', { name }));
      continue;
    }
    for (const s of a.skills.filter((x) => !x.installed)) {
      const label =
        preview?.agents.find((p) => p.name === a.name)?.skills?.find((x) => x.name === s.name)
          ?.description || s.name;
      lines.push(t('experts.install.result.skillMissing', { name, skill: label }));
    }
    if (a.contract === false) {
      lines.push(t('experts.install.result.contractMissing', { name }));
    }
  }
  for (const w of placement.wiki.filter((x) => x.status !== 'installed')) {
    const title = preview?.wiki_pages?.find((p) => p.file === w.file)?.title || w.file;
    lines.push(t(`experts.install.result.wiki.${w.status}`, { name: title }));
  }
  return lines;
}

/** Post-install summary shown when something was not placed. */
export function InstallResult({
  placement,
  preview,
}: {
  placement: ExpertInstallPlacement;
  preview: ExpertDraftPreview | null;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);
  const problems = placementProblems(placement, preview, t);
  const installedAgents = placement.agents.filter((a) => a.installed).length;
  const installedPages = placement.wiki.filter((w) => w.status === 'installed').length;
  return (
    <div className="space-y-3">
      <p className="flex items-center gap-2 text-sm">
        <Check className="size-4 text-success" aria-hidden />
        {t('experts.install.result.summary', {
          agents: installedAgents,
          pages: installedPages,
        })}
      </p>
      {problems.length > 0 && (
        <div className="rounded-lg border border-warning/30 bg-warning/10 p-3 text-xs">
          <p className="flex items-center gap-1.5 font-medium text-warning">
            <AlertTriangle className="size-3.5" aria-hidden />
            {t('experts.install.result.problemsTitle')}
          </p>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {problems.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
