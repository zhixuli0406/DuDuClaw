export declare const UNICODE_EXTENSION_SEQUENCE_REGEX: RegExp;
/**
* Asserts that a condition is true, throwing an error if it is not.
* Used for runtime validation and type narrowing.
*
* @param condition - The condition to check
* @param message - Error message if condition is false
* @param Err - Error constructor to use (defaults to Error)
* @throws {Error} When condition is false
*
* @example
* ```ts
* invariant(locale !== undefined, 'Locale must be defined')
* // locale is now narrowed to non-undefined type
* ```
*/
export declare function invariant(condition: boolean, message: string, Err?: any): asserts condition;
/**
* Calculates the matching distance between two locales using the CLDR Enhanced Language Matching algorithm.
* This function is memoized for performance, as distance calculations are expensive.
*
* The distance represents how "far apart" two locales are, with 0 being identical (after maximization).
* Distances are calculated based on Language-Script-Region (LSR) differences using CLDR data.
*
* @param desired - The desired locale (e.g., "en-US")
* @param supported - The supported locale to compare against (e.g., "en-GB")
* @returns The calculated distance between the locales
*
* @example
* ```ts
* findMatchingDistance('en-US', 'en-US') // 0 - identical
* findMatchingDistance('en-US', 'en-GB') // 40 - same language/script, different region
* findMatchingDistance('es-CO', 'es-419') // 39 - regional variant
* findMatchingDistance('en', 'fr') // 840 - completely different languages
* ```
*
* @see https://unicode.org/reports/tr35/#EnhancedLanguageMatching
*/
export declare const findMatchingDistance: (desired: string, supported: string) => number;
interface LocaleMatchingResult {
	distances: Record<string, Record<string, number>>;
	matchedSupportedLocale?: string;
	matchedDesiredLocale?: string;
}
export declare function findBestMatch(requestedLocales: readonly string[], supportedLocales: readonly string[], threshold?: number): LocaleMatchingResult;
export {};
