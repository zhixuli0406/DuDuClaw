import { create } from 'zustand';

export interface CompanyInfo { name: string; logo_url: string | null }
const emptyCompany: CompanyInfo = { name: '', logo_url: null };

export function parseCompany(value: unknown): CompanyInfo {
  if (!value || typeof value !== 'object') return emptyCompany;
  const data = value as Record<string, unknown>;
  if (typeof data.name !== 'string' || [...data.name].length > 120
    || /[\p{Cc}\p{Cf}\u2028\u2029]/u.test(data.name)) return emptyCompany;
  return {
    name: data.name,
    logo_url: typeof data.logo_url === 'string' && /^\/api\/company\/logo\/[a-f0-9]{64}\.png$/.test(data.logo_url)
      ? data.logo_url : null,
  };
}

let requestEpoch = 0;
export const useCompanyStore = create<{
  company: CompanyInfo;
  setCompany: (value: unknown) => void;
}>((set) => ({
  company: emptyCompany,
  setCompany: (value) => {
    requestEpoch += 1;
    set({ company: parseCompany(value) });
  },
}));

export async function loadPublicCompany(): Promise<void> {
  const epoch = ++requestEpoch;
  try {
    const response = await fetch('/api/company', { cache: 'no-store' });
    const value: unknown = response.ok ? await response.json() : null;
    if (epoch === requestEpoch) useCompanyStore.getState().setCompany(value);
  } catch {
    if (epoch === requestEpoch) useCompanyStore.getState().setCompany(null);
  }
}
