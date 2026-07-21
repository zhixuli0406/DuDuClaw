/**
 * True while an IME (Chinese / Japanese / Korean input method) is mid-composition.
 *
 * During composition the first Enter confirms the highlighted candidate — it
 * must NOT trigger a submit/confirm handler, or CJK users lose half of what they
 * typed (the reported 注音/拼音 "Enter sends a half-composed message" bug). Call
 * this in any keydown handler that acts on Enter over a free-text field:
 *
 *   if (e.key === 'Enter' && !isImeComposing(e)) submit();
 *
 * Two signals are checked because browsers disagree on timing:
 *  - `isComposing` — Chrome / Firefox fire the confirming keydown *with*
 *    `isComposing === true` before `compositionend`.
 *  - `keyCode === 229` — Safari / WebKit fire the confirming keydown *after*
 *    `compositionend` with the 229 "processing" sentinel keyCode instead.
 *
 * Accepts both a React `KeyboardEvent` (reads `nativeEvent.isComposing`) and a
 * native DOM `KeyboardEvent` (reads `isComposing` directly).
 */
export function isImeComposing(e: {
  readonly keyCode?: number;
  readonly isComposing?: boolean;
  readonly nativeEvent?: { readonly isComposing?: boolean };
}): boolean {
  const composing = e.isComposing ?? e.nativeEvent?.isComposing ?? false;
  return composing || e.keyCode === 229;
}
