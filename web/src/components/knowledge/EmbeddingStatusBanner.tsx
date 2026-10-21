import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Info } from 'lucide-react';
import { knowledgeApi, type KbEmbeddingStatus } from '@/lib/knowledge-api';

/**
 * EmbeddingStatusBanner — says how search works right now. While only
 * keyword matching is available the banner explains what that means for
 * the person asking (the question has to share words with the document).
 * Renders nothing when meaning-based search is on, or when the status could
 * not be read (the rest of the page still works).
 */
export function EmbeddingStatusBanner() {
  const intl = useIntl();
  const [status, setStatus] = useState<KbEmbeddingStatus | null>(null);

  useEffect(() => {
    let alive = true;
    knowledgeApi
      .embeddingStatus()
      .then((s) => alive && setStatus(s))
      .catch(() => {
        /* Non-essential: no banner. */
      });
    return () => {
      alive = false;
    };
  }, []);

  if (!status || status.available) return null;
  return (
    <div
      role="note"
      className="flex items-start gap-2 rounded-lg border border-surface-border bg-muted/40 px-3 py-2.5 text-sm text-muted-foreground"
    >
      <Info aria-hidden className="mt-0.5 size-4 shrink-0" />
      <p>{intl.formatMessage({ id: 'kb.embedding.keywordOnly' })}</p>
    </div>
  );
}
