import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, ReceiptText } from 'lucide-react';
import { forgetReceipts, type ForgetReceipt } from '@/lib/forget-receipts';
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
      if (request === generation.current) setReceipts(result.receipts);
    } catch (err) {
      if (request === generation.current) {
        setReceipts([]);
        setError(err);
      }
    } finally {
      if (request === generation.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (connection === 'authenticated') void load();
    else setReceipts([]);
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
                          {['employee', 'memories', 'keyFacts', 'archives', 'unattributed', 'systemKept'].map((key) => (
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
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </div>
                  <p className="text-xs text-muted-foreground">{t('unattributedHint')}</p>
                  <Button size="sm" variant="outline" disabled={downloading != null} onClick={() => void download(receipt.id)}>
                    <Download className="mr-1 size-3" aria-hidden />
                    {t('download')}
                  </Button>
                </div>
              </details>
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
