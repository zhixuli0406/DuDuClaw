/**
* CanonicalizeTimeZoneName ( timeZone )
* https://tc39.es/ecma402/#sec-canonicalizetimezonename
*
* Extended to support UTC offset time zones per ECMA-402 PR #788 (ES2026).
* Returns the canonical and case-regularized form of a timezone identifier.
*
* @param tz - The timezone identifier to canonicalize
* @param implDetails - Implementation details containing timezone data
* @returns The canonical timezone identifier
*/
export declare function CanonicalizeTimeZoneName(tz: string, { zoneNames, uppercaseLinks }: {
	zoneNames: readonly string[];
	uppercaseLinks: Record<string, string>;
}): string;
