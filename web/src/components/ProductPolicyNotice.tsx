import { useIntl } from 'react-intl';
import { Button } from '@/components/mds';
import { useProductPolicy } from '@/lib/product-policy';
/** The policy RPC is authoritative. Never reopen restricted pickers after a read failure. */
export function ProductPolicyNotice() {
  const policy=useProductPolicy(); const intl=useIntl();
  if(policy.policy) return null;
  return <div role={policy.error?'alert':'status'} className="flex flex-wrap items-center gap-2 rounded border border-surface-border p-3 text-xs text-muted-foreground">
    {intl.formatMessage({id:policy.error?'productPolicy.error':'productPolicy.loading'})}
    {policy.error && <Button size="sm" variant="outline" onClick={()=>void policy.load()}>{intl.formatMessage({id:'errorState.retry'})}</Button>}
  </div>;
}
