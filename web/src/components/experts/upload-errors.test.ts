import { describe, it, expect } from 'vitest';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';
import { UPLOAD_ERROR_CODES, UploadError, uploadErrorKey } from './upload-errors';
import { KNOWN_ERROR_CODES } from './ImportErrorPanel';

describe('upload / import error catalogue', () => {
  it('maps upload codes, falls back by HTTP status for older gateways', () => {
    expect(uploadErrorKey(new UploadError('upload.forbidden', 403))).toBe('experts.upload.error.forbidden');
    expect(uploadErrorKey(new UploadError('', 413))).toBe('experts.upload.error.too_large');
    expect(uploadErrorKey(new UploadError('', 401))).toBe('experts.upload.error.unauthorized');
    expect(uploadErrorKey(new UploadError('upload.whatever', 500))).toBe('experts.upload.error.generic');
    expect(uploadErrorKey(new Error('x'))).toBe('experts.upload.error.generic');
  });

  it('every code has plain-language text and a fix in all three languages', () => {
    for (const cat of [en, zhTW, jaJP] as Record<string, string>[]) {
      for (const code of KNOWN_ERROR_CODES) {
        expect(cat[`experts.error.${code}.text`], code).toBeTruthy();
        expect(cat[`experts.error.${code}.fix`], code).toBeTruthy();
      }
      for (const code of UPLOAD_ERROR_CODES) {
        expect(cat[`experts.upload.error.${code.slice('upload.'.length)}`], code).toBeTruthy();
      }
    }
  });

  it('no message names a terminal command', () => {
    for (const cat of [en, zhTW, jaJP] as Record<string, string>[]) {
      for (const [k, v] of Object.entries(cat)) {
        if (!k.startsWith('experts.error.') && !k.startsWith('experts.reimport.') && !k.startsWith('experts.upload.error.')) continue;
        expect(v, k).not.toMatch(/duduclaw |--[a-z]|expert remove|pack install/);
      }
    }
  });
});
