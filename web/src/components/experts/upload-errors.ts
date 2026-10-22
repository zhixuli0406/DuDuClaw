/** Codes `POST /api/experts/upload` returns (`{ error, code }`). */
export const UPLOAD_ERROR_CODES = [
  'upload.unauthorized',
  'upload.forbidden',
  'upload.malformed',
  'upload.too_large',
  'upload.interrupted',
  'upload.no_file',
  'upload.not_zip',
  'upload.not_text',
  'upload.write_failed',
  'upload.network',
] as const;

/** A rejected upload: the gateway's error code and the HTTP status. */
export class UploadError extends Error {
  readonly code: string;
  readonly status: number;

  constructor(code: string, status: number) {
    super(code || `HTTP ${status}`);
    this.code = code;
    this.status = status;
  }
}

/** The message id explaining a failed upload in plain language. Older
 *  gateways send no code: 401/403/413 still map by status. */
export function uploadErrorKey(e: unknown): string {
  if (!(e instanceof UploadError)) return 'experts.upload.error.generic';
  if ((UPLOAD_ERROR_CODES as readonly string[]).includes(e.code)) {
    return `experts.upload.error.${e.code.slice('upload.'.length)}`;
  }
  switch (e.status) {
    case 401:
      return 'experts.upload.error.unauthorized';
    case 403:
      return 'experts.upload.error.forbidden';
    case 413:
      return 'experts.upload.error.too_large';
    default:
      return 'experts.upload.error.generic';
  }
}
