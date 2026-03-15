/**
* IsValidTimeZoneName ( timeZone )
* https://tc39.es/ecma402/#sec-isvalidtimezonename
*
* Extended to support UTC offset time zones per ECMA-402 PR #788 (ES2026).
* The abstract operation validates both:
* 1. UTC offset identifiers (e.g., "+01:00", "-05:30")
* 2. Available named time zone identifiers from IANA Time Zone Database
*
* @param tz - The timezone identifier to validate
* @param implDetails - Implementation details containing timezone data
* @returns true if timeZone is a valid identifier
*/
export declare function IsValidTimeZoneName(tz: string, { zoneNamesFromData, uppercaseLinks }: {
	zoneNamesFromData: readonly string[];
	uppercaseLinks: Record<string, string>;
}): boolean;
