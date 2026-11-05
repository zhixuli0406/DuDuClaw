// WebAuthn transports binary fields as base64url; the browser owns all crypto.
export function decodeBase64url(value: string): ArrayBuffer {
  const decoded = atob(value.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(decoded, c => c.charCodeAt(0)).buffer;
}
export function encodeBase64url(value: ArrayBuffer): string {
  return btoa(Array.from(new Uint8Array(value), byte => String.fromCharCode(byte)).join(''))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
type DescriptorJSON = Omit<PublicKeyCredentialDescriptor, 'id'> & { id: string };
export type CreationOptionsJSON = Omit<PublicKeyCredentialCreationOptions, 'challenge' | 'user' | 'excludeCredentials'> & {
  challenge: string; user: Omit<PublicKeyCredentialUserEntity, 'id'> & { id: string }; excludeCredentials?: DescriptorJSON[];
};
export type RequestOptionsJSON = Omit<PublicKeyCredentialRequestOptions, 'challenge' | 'allowCredentials'> & {
  challenge: string; allowCredentials?: DescriptorJSON[];
};
const descriptors = (values?: DescriptorJSON[]) => values?.map(value => ({ ...value, id: decodeBase64url(value.id) }));
export function passkeyAvailable(): boolean {
  return window.isSecureContext && typeof PublicKeyCredential !== 'undefined' && !!navigator.credentials;
}
function serialize(credential: PublicKeyCredential) {
  const response = credential.response;
  const base = { id: credential.id, rawId: encodeBase64url(credential.rawId), type: credential.type,
    extensions: credential.getClientExtensionResults() };
  if (response instanceof AuthenticatorAttestationResponse) return { ...base, response: {
    clientDataJSON: encodeBase64url(response.clientDataJSON), attestationObject: encodeBase64url(response.attestationObject),
    transports: response.getTransports?.() ?? [],
  } };
  const assertion = response as AuthenticatorAssertionResponse;
  return { ...base, response: { clientDataJSON: encodeBase64url(assertion.clientDataJSON),
    authenticatorData: encodeBase64url(assertion.authenticatorData), signature: encodeBase64url(assertion.signature),
    userHandle: assertion.userHandle ? encodeBase64url(assertion.userHandle) : null } };
}
export async function createPasskey(options: CreationOptionsJSON) {
  if (!passkeyAvailable()) throw new Error('PASSKEY_UNAVAILABLE');
  const credential = await navigator.credentials.create({ publicKey: { ...options,
    challenge: decodeBase64url(options.challenge), user: { ...options.user, id: decodeBase64url(options.user.id) },
    excludeCredentials: descriptors(options.excludeCredentials) } });
  if (!(credential instanceof PublicKeyCredential)) throw new Error('PASSKEY_CANCELLED');
  return serialize(credential);
}
export async function authenticatePasskey(options: RequestOptionsJSON) {
  if (!passkeyAvailable()) throw new Error('PASSKEY_UNAVAILABLE');
  const credential = await navigator.credentials.get({ publicKey: { ...options,
    challenge: decodeBase64url(options.challenge), allowCredentials: descriptors(options.allowCredentials) } });
  if (!(credential instanceof PublicKeyCredential)) throw new Error('PASSKEY_CANCELLED');
  return serialize(credential);
}
