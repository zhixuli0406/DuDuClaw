import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { Info } from 'lucide-react';
import { knowledgeApi, type KbEmbeddingStatus } from '@/lib/knowledge-api';
import { useAuthStore } from '@/stores/auth-store';

/**
 * EmbeddingStatusBanner — says how search works right now. While only
 * keyword matching is available the banner explains what that means for
 * the person asking (the question has to share words with the document).
 * Renders nothing when meaning-based search is on, or when the status could
 * not be read (the rest of the page still works). Admins also get the
 * reason and a link to the one place it is set up (Settings → Knowledge
 * base search); this banner is not a second settings surface.
 */
export function EmbeddingStatusBanner() {
  const intl = useIntl();
  const [status, setStatus] = useState<KbEmbeddingStatus | null>(null);
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');

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
      <div className="space-y-1">
        <p>{intl.formatMessage({ id: 'kb.embedding.keywordOnly' })}</p>
        {isAdmin && (
          <p>
            {status.reason ? `${status.reason}　` : ''}
            <Link to="/settings?tab=knowledgeSearch" className="font-medium text-brand underline-offset-4 hover:underline">
              {intl.formatMessage({ id: 'kb.embedding.setUp' })}
            </Link>
          </p>
        )}
      </div>
    </div>
  );
}
