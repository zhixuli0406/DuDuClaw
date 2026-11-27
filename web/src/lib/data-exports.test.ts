import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { exportDateRange, downloadDataExport } from './data-exports';
import { resetCompanyTimeZone, useCompanyTimeStore } from './company-time';
import { useAuthStore } from '@/stores/auth-store';
describe('data exports', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    useCompanyTimeStore.setState({ status: 'set', zone: 'Asia/Taipei' });
  });
  afterEach(() => { resetCompanyTimeZone(); });
  it('uses Taipei half-open dates, including end day', () => {
    expect(exportDateRange('2026-11-27','2026-11-27')).toEqual({from:'2026-11-26T16:00:00.000Z',until:'2026-11-27T16:00:00.000Z'});
    expect(() => exportDateRange('2026-10-12','2026-10-11')).toThrow('EXPORT_INVALID_DATE');
  });
  it('uses the next local midnight across a 23-hour DST day', () => {
    const range = exportDateRange('2026-03-08', '2026-03-08', 'America/New_York');
    expect(range).toEqual({ from: '2026-03-08T05:00:00.000Z', until: '2026-03-09T04:00:00.000Z' });
    expect(Date.parse(range.until!) - Date.parse(range.from!)).toBe(23 * 3600_000);
  });
  it('also handles a 25-hour DST day using the company zone', () => {
    useCompanyTimeStore.setState({ status: 'set', zone: 'America/New_York' });
    expect(exportDateRange('2026-11-01', '2026-11-01')).toEqual({
      from: '2026-11-01T04:00:00.000Z', until: '2026-11-02T05:00:00.000Z',
    });
  });
  it('falls back to the browser calendar when the company zone is unset', () => {
    resetCompanyTimeZone();
    expect(exportDateRange('2026-11-27', '2026-11-27')).toEqual({
      from: new Date('2026-11-27T00:00:00').toISOString(),
      until: new Date('2026-11-28T00:00:00').toISOString(),
    });
  });
  it('preserves open ranges', () => {
    expect(exportDateRange('', '')).toEqual({});
    expect(exportDateRange('2026-11-27', '')).toEqual({ from: '2026-11-26T16:00:00.000Z' });
    expect(exportDateRange('', '2026-11-27')).toEqual({ until: '2026-11-27T16:00:00.000Z' });
  });
  it.each(['2026-02-30', 'not-a-date', '2026-1-01'])('keeps the validation error for %s', (date) => {
    expect(() => exportDateRange(date, '')).toThrow('EXPORT_INVALID_DATE');
    expect(() => exportDateRange('', date)).toThrow('EXPORT_INVALID_DATE');
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
