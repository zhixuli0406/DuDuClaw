import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, ReceiptText } from 'lucide-react';
import { forgetReceipts, type ForgetReceipt, type ForgetJob } from '@/lib/forget-receipts';
import { useConnectionStore } from '@/stores/connection-store';
import { formatError, toast } from '@/lib/toast';
import {
  Button, Card, CardContent, CardDescription, CardHeader, CardTitle,
  Empty, ErrorState, Skeleton, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from '@/components/mds';

export function ForgetReceiptsPanel() {
  const intl = useIntl();
  const connection = useConnectionStore((s) => s.state);
  const [receipts, setReceipts] = useState<ForgetReceipt[]>([]);
  const [jobs, setJobs] = useState<ForgetJob[]>([]);
  const [retrying, setRetrying] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [downloading, setDownloading] = useState<string | null>(null);
  const generation = useRef(0);
  const t = (id: string) => intl.formatMessage({ id: `forget.receipts.${id}` });

  const load = useCallback(async () => {
    const request = ++generation.current;
    setLoading(true);
    setError(null);
    try {
      const result = await forgetReceipts.list();
      if (!Array.isArray(result.receipts)) throw new Error('Invalid receipt list');
      if (request === generation.current) {
        setReceipts(result.receipts);
        setJobs(Array.isArray(result.jobs) ? result.jobs : []);
      }
    } catch (err) {
      if (request === generation.current) {
        setReceipts([]);
        setJobs([]);
        setError(err);
      }
    } finally {
      if (request === generation.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    else { setReceipts([]); setJobs([]); }
    return () => { generation.current += 1; };
  }, [connection, load]);

  const download = async (id: string) => {
    setDownloading(id);
    try {
      const { receipt } = await forgetReceipts.get(id);
      if (!receipt || receipt.id !== id) throw new Error('Invalid receipt');
      const url = URL.createObjectURL(new Blob([JSON.stringify(receipt, null, 2)], { type: 'application/json' }));
      const link = document.createElement('a');
      link.href = url;
      link.download = `forget-receipt-${id.replace(/[^a-zA-Z0-9_-]/g, '_')}.json`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      const revoke = URL.revokeObjectURL.bind(URL);
      setTimeout(() => revoke(url), 1000);
    } catch (err) {
      toast.error(intl.formatMessage({ id: 'forget.receipts.downloadFailed' }, { message: formatError(err) }));
    } finally {
      setDownloading(null);
    }
  };

  const retry = async (id: string) => {
    setRetrying(id);
    try {
      await forgetReceipts.retry(id);
      await load();
    } catch (err) {
      toast.error(intl.formatMessage({ id: 'forget.receipts.retryFailed' }, { message: formatError(err) }));
    } finally {
      setRetrying(null);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <ReceiptText className="size-4 text-brand" aria-hidden />
          {t('title')}
        </CardTitle>
        <CardDescription>{t('description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">{t('outOfScope')}</p>
        {connection !== 'authenticated' ? (
          <p className="text-sm text-muted-foreground">{t('disconnected')}</p>
        ) : error != null ? (
          <ErrorState variant="inline" error={error} title={t('loadFailed')} onRetry={() => void load()} />
        ) : loading ? (
          <Skeleton className="h-24 w-full" />
        ) : receipts.length === 0 ? (
          <Empty icon={ReceiptText} title={t('empty')} description={t('emptyDescription')} />
        ) : (
          <div className="space-y-3">
            {receipts.map((receipt) => (
              <details key={receipt.id} className="rounded-lg border border-surface-border p-3">
                <summary className="cursor-pointer text-sm font-medium">
                  {receipt.display_name} · {receipt.completed_at}
                </summary>
                <div className="mt-3 space-y-3">
                  <p className="break-all text-xs text-muted-foreground">
                    {t('approvedBy')}: {receipt.approved_by} · {t('batch')}: {receipt.batch_id}
                  </p>
                  <div className="overflow-x-auto">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          {['employee', 'memories', 'keyFacts', 'archives', 'unattributed', 'systemKept', 'retainedShared'].map((key) => (
                            <TableHead key={key}>{t(key)}</TableHead>
                          ))}
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {receipt.counts.map((count) => (
                          <TableRow key={count.agent_id}>
                            <TableCell>{count.agent_id}</TableCell>
                            <TableCell>{count.memories_deleted}</TableCell>
                            <TableCell>{count.key_facts_deleted}</TableCell>
                            <TableCell>{count.archive_deleted}</TableCell>
                            <TableCell>{count.unattributed}</TableCell>
                            <TableCell>{count.system_kept}</TableCell>
                            <TableCell>{count.retained_shared ?? '—'}</TableCell>
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </div>
                  <p className="text-xs text-muted-foreground">{t('unattributedHint')}</p>
                  <p className="text-xs text-muted-foreground">{t('retainedSharedHint')}</p>
                  <Button size="sm" variant="outline" disabled={downloading != null} onClick={() => void download(receipt.id)}>
                    <Download className="mr-1 size-3" aria-hidden />
                    {t('download')}
                  </Button>
                </div>
              </details>
            ))}
          </div>
        )}
        {connection === 'authenticated' && !loading && error == null && jobs.length > 0 && (
          <div className="space-y-3">
            <h3 className="text-sm font-medium">{t('jobsTitle')}</h3>
            {jobs.map((job) => (
              <div key={job.id} className="space-y-2 rounded-lg border border-surface-border p-3">
                <p className="text-sm font-medium">{job.display_name} · {t(`job.${job.status}`)}</p>
                <p className="break-all text-xs text-muted-foreground">{t('batch')}: {job.id}</p>
                <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'forget.receipts.attempts' }, { count: job.attempt_count })}</p>
                {job.last_error && <p className="text-sm">{t('failureReason')}: {job.last_error}</p>}
                {job.status === 'quarantined' ? (
                  <>
                    <p className="text-xs text-muted-foreground">{t('quarantinedHint')}</p>
                    <Button size="sm" variant="outline" disabled={retrying != null} onClick={() => void retry(job.id)}>
                      {t('retry')}
                    </Button>
                  </>
                ) : job.next_attempt_at > 0 && (
                  <p className="text-xs text-muted-foreground">{t('nextAttempt')}: {new Date(job.next_attempt_at * 1000).toLocaleString()}</p>
                )}
              </div>
            ))}
          </div>
        )}
        <div className="flex justify-end">
          <Button size="sm" variant="outline" onClick={() => void load()} disabled={loading || connection !== 'authenticated'}>
            {t('refresh')}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
