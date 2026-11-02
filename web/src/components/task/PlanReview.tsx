import { useIntl } from 'react-intl';

type Lens = 'restate' | 'simplify' | 'ready';
type LensResult = { lens: Lens; status: 'completed' | 'not_run'; text: string };

/** Untrusted review output is display-only and never interpreted as markup. */
function parseReview(raw: string | null | undefined): LensResult[] | null {
  if (!raw) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== 'object' || value === null) return null;
    const review = value as { version?: unknown; lenses?: unknown };
    if (review.version !== 1 || !Array.isArray(review.lenses) || review.lenses.length !== 3) return null;
    const seen = new Set<string>();
    for (const item of review.lenses) {
      if (typeof item !== 'object' || item === null ||
          !['restate', 'simplify', 'ready'].includes(item.lens) ||
          !['completed', 'not_run'].includes(item.status) ||
          typeof item.text !== 'string' || seen.has(item.lens)) return null;
      seen.add(item.lens);
    }
    return review.lenses as LensResult[];
  } catch {
    return null;
  }
}

export function PlanReview({ review }: { review?: string | null }) {
  const intl = useIntl();
  const lenses = parseReview(review);
  if (!lenses) return null;
  return (
    <details className="rounded-lg border border-surface-border px-3 py-2 text-sm">
      <summary className="cursor-pointer font-medium">
        {intl.formatMessage({ id: 'tasks.planReview.title' })}
      </summary>
      <dl className="mt-3 space-y-3">
        {lenses.map(({ lens, status, text }) => (
          <div key={lens}>
            <dt className="font-medium">{intl.formatMessage({ id: `tasks.planReview.${lens}` })}</dt>
            <dd className="whitespace-pre-wrap break-words text-muted-foreground">
              {status === 'not_run' ? intl.formatMessage({ id: 'tasks.planReview.notRun' }) : text}
            </dd>
          </div>
        ))}
      </dl>
    </details>
  );
}
