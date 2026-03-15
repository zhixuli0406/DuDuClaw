class MissingLocaleDataError extends Error {
	type = "MISSING_LOCALE_DATA";
}
export function isMissingLocaleDataError(e) {
	return e.type === "MISSING_LOCALE_DATA";
}
