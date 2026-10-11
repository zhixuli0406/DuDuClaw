import { describe, it, expect, vi, beforeEach } from 'vitest';
import { exportDateRange, downloadDataExport } from './data-exports';
import { useAuthStore } from '@/stores/auth-store';
describe('data exports', () => {
  beforeEach(() => { vi.unstubAllGlobals(); });
  it('uses Taipei half-open dates, including end day', () => {
    expect(exportDateRange('2026-10-11','2026-10-11')).toEqual({from:'2026-10-10T16:00:00.000Z',until:'2026-10-11T16:00:00.000Z'});
    expect(() => exportDateRange('2026-10-12','2026-10-11')).toThrow();
  });
  it('downloads with a bearer header and never a URL token', async () => {
    useAuthStore.setState({jwt:'secret-token'});
    const fetcher=vi.fn().mockResolvedValue({ok:false,status:403}); vi.stubGlobal('fetch',fetcher);
    await expect(downloadDataExport('export-123','tasks','csv')).rejects.toThrow('EXPORT_FORBIDDEN');
    expect(fetcher).toHaveBeenCalledWith('/api/data-exports/export-123/download',expect.objectContaining({headers:{Authorization:'Bearer secret-token'}}));
    expect(fetcher.mock.calls[0][0]).not.toContain('secret-token');
  });
  it('rejects a successful HTML fallback rather than presenting it as Excel', async () => {
    useAuthStore.setState({jwt:'jwt'}); vi.stubGlobal('fetch',vi.fn().mockResolvedValue({ok:true,headers:new Headers({'content-type':'text/html'})}));
    await expect(downloadDataExport('export-123','wiki','xlsx')).rejects.toThrow('EXPORT_DOWNLOAD_FAILED');
  });
  it('clearly identifies expired artifacts', async () => {
    useAuthStore.setState({jwt:'jwt'}); vi.stubGlobal('fetch',vi.fn().mockResolvedValue({ok:false,status:410}));
    await expect(downloadDataExport('export-123','wiki','xlsx')).rejects.toThrow('EXPORT_EXPIRED');
  });
});

it('preserves a source-change error instead of reporting expiry', async()=>{
 useAuthStore.setState({jwt:'jwt'});
 vi.stubGlobal('fetch',vi.fn().mockResolvedValue({ok:false,status:410,json:async()=>({error:'EXPORT_SOURCE_CHANGED'})}));
 await expect(downloadDataExport('export-123','wiki','xlsx')).rejects.toThrow('EXPORT_SOURCE_CHANGED');
});
