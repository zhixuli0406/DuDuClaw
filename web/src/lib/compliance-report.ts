import { client } from './ws-client';
import { exportDateRange } from './data-exports';

export type ComplianceFormat = 'csv' | 'xlsx' | 'html';
export type ComplianceLanguage = 'zh-TW' | 'en' | 'ja-JP';
export interface ComplianceRequest {
  from: string; until: string; language: ComplianceLanguage;
  format: ComplianceFormat; include_personal_data: boolean;
}
export interface ComplianceReport {
  filename: string; content_type: string; content_base64: string;
  outbound_count: number; approval_count: number; access_count: number;
}
export const complianceReports = {
  generate: (request: ComplianceRequest) => client.call('compliance.generate', { ...request }) as Promise<ComplianceReport>,
};
export function complianceDateRange(start: string, end: string): { from: string; until: string } {
  const range = exportDateRange(start, end);
  if (!range.from || !range.until || Date.parse(range.until) - Date.parse(range.from) > 366 * 86400_000) {
    throw new Error('COMPLIANCE_INVALID_REQUEST');
  }
  return { from: range.from, until: range.until };
}
export function complianceLanguage(locale: string): ComplianceLanguage {
  return locale === 'en' ? 'en' : locale === 'ja-JP' ? 'ja-JP' : 'zh-TW';
}
export function downloadComplianceReport(report: ComplianceReport, format: ComplianceFormat): void {
  const mime = { csv: 'text/csv', xlsx: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet', html: 'text/html' }[format];
  if (report.content_type.split(';')[0].trim() !== mime || report.content_base64.length > 24 * 1024 * 1024) {
    throw new Error('COMPLIANCE_DOWNLOAD_FAILED');
  }
  const binary = atob(report.content_base64);
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  const url = URL.createObjectURL(new Blob([bytes], { type: report.content_type }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = `duduclaw-compliance.${format}`;
  document.body.append(anchor); anchor.click(); anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
