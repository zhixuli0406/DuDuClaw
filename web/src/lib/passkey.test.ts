import { afterEach, describe, expect, it, vi } from 'vitest';
import { authenticatePasskey, decodeBase64url, encodeBase64url, passkeyAvailable, passkeyUnavailableReason } from './passkey';

afterEach(() => vi.unstubAllGlobals());

describe('WebAuthn binary transport', () => {
  it('preserves challenge bytes including URL-unsafe base64 characters', () => {
    const bytes = new Uint8Array([0, 255, 254, 253, 128, 1]).buffer;
    const encoded = encodeBase64url(bytes);
    expect(encoded).not.toMatch(/[+/=]/);
    expect(new Uint8Array(decodeBase64url(encoded))).toEqual(new Uint8Array(bytes));
  });
  it('rejects malformed base64 instead of fabricating challenge bytes', () => {
    expect(() => decodeBase64url('!invalid!')).toThrow();
  });
});

it('sends one extensions field compatible with the server aliases and binary response fields', async () => {
  const bytes = new Uint8Array([1, 2, 3]).buffer;
  class Credential {
    id = 'AQID'; rawId = bytes; type = 'public-key';
    response = { clientDataJSON: bytes, authenticatorData: bytes, signature: bytes, userHandle: null };
    getClientExtensionResults() { return {}; }
  }
  const get = vi.fn().mockResolvedValue(new Credential());
  vi.stubGlobal('PublicKeyCredential', Credential);
  vi.stubGlobal('AuthenticatorAttestationResponse', class {});
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('navigator', { credentials: { get } });
  const result = await authenticatePasskey({ challenge: 'AQID', rpId: 'example.com', userVerification: 'required', allowCredentials: [{ type: 'public-key', id: 'AQID' }] });
  expect(result.extensions).toEqual({});
  expect(result).not.toHaveProperty('clientExtensionResults');
  expect(result.response).toMatchObject({ clientDataJSON: 'AQID', signature: 'AQID', userHandle: null });
  expect(get).toHaveBeenCalledWith(expect.objectContaining({ publicKey: expect.objectContaining({ challenge: bytes, allowCredentials: [{ type: 'public-key', id: bytes }] }) }));
});

it('distinguishes insecure contexts, unsupported browsers, and usable WebAuthn', () => {
  vi.stubGlobal('isSecureContext', false);
  expect(passkeyUnavailableReason()).toBe('insecure_context');
  expect(passkeyAvailable()).toBe(false);
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('PublicKeyCredential', undefined);
  expect(passkeyUnavailableReason()).toBe('unsupported_browser');
  vi.stubGlobal('PublicKeyCredential', class {});
  vi.stubGlobal('navigator', { credentials: {} });
  expect(passkeyUnavailableReason()).toBeNull();
  expect(passkeyAvailable()).toBe(true);
});
