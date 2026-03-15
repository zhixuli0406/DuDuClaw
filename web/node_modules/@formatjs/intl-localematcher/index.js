import { CanonicalizeLocaleList } from "./abstract/CanonicalizeLocaleList.js";
import { ResolveLocale } from "./abstract/ResolveLocale.js";
export function match(requestedLocales, availableLocales, defaultLocale, opts) {
	return ResolveLocale(availableLocales, CanonicalizeLocaleList(requestedLocales), { localeMatcher: opts?.algorithm || "best fit" }, [], {}, () => defaultLocale).locale;
}
export { LookupSupportedLocales } from "./abstract/LookupSupportedLocales.js";
export { ResolveLocale } from "./abstract/ResolveLocale.js";
