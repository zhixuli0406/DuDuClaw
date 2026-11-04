import { afterEach, describe, expect, it, vi } from 'vitest';
import { chunkSha256, sha256Fallback } from './sha256';
const bytes = (text: string) => new TextEncoder().encode(text).buffer as ArrayBuffer;
afterEach(() => vi.unstubAllGlobals());
describe('HTTP LAN chunk hashing', () => {
  it('matches empty, abc and multi-block SHA-256 known vectors', () => {
    expect(sha256Fallback(bytes(''))).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    expect(sha256Fallback(bytes('abc'))).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
    expect(sha256Fallback(bytes('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq'))).toBe('248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');
  });
  it('hashes all binary byte values when WebCrypto is missing', async () => {
    vi.stubGlobal('crypto', {});
    const binary = Uint8Array.from({ length: 256 }, (_, i) => i).buffer;
    expect(await chunkSha256(binary)).toBe('40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880');
  });
  it('bounds fallback memory to the upload chunk limit', () => {
    expect(() => sha256Fallback(new ArrayBuffer(8 * 1024 * 1024 + 1))).toThrow();
  });
});
