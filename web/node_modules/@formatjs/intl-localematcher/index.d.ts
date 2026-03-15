export interface Opts {
	algorithm: "lookup" | "best fit";
}
export declare function match(requestedLocales: readonly string[], availableLocales: readonly string[], defaultLocale: string, opts?: Opts): string;
export { LookupSupportedLocales } from "./abstract/LookupSupportedLocales.js";
export { ResolveLocale } from "./abstract/ResolveLocale.js";
