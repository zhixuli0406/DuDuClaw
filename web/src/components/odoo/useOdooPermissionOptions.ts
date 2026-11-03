import { useEffect, useState } from 'react';
import { api, type OdooPermissionOptions } from '@/lib/api';

export function useOdooPermissionOptions(enabled = true) {
  const [options, setOptions] = useState<OdooPermissionOptions>({ models: [], blocked_models: [], read_only_models: [], scan_available: false });
  const [policyReady, setPolicyReady] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    // The policy is local and can arrive before connection-dependent cache lookup.
    void api.odoo.config().then((config) => {
      if (active && config?.blocked_models && config.read_only_models) {
        setOptions((prev) => ({ ...prev, blocked_models: config.blocked_models!, read_only_models: config.read_only_models! }));
        setPolicyReady(true);
      }
    }).catch(() => {});
    void api.odoo.permissionOptions().then((result) => {
      if (!active) return;
      if (Array.isArray(result.blocked_models) && Array.isArray(result.read_only_models)) {
        setOptions(result);
        setPolicyReady(true);
      } else setFailed(true);
    }).catch(() => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [enabled]);

  async function scan() {
    setScanning(true);
    setFailed(false);
    try {
      const result = await api.odoo.scanSchema();
      if (!result.success) { setFailed(true); return; }
      const refreshed = await api.odoo.permissionOptions();
      setOptions(refreshed);
      setPolicyReady(true);
      setFailed(!refreshed.scan_available);
    } catch { setFailed(true); }
    finally { setScanning(false); }
  }
  return { options, policyReady, scanning, failed, scan };
}
