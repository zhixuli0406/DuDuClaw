import { ErrorCode, FormatError } from "intl-messageformat";
import { IntlFormatError } from "./error.js";
import "./types.js";
import { filterProps } from "./utils.js";
const PLURAL_FORMAT_OPTIONS = ["type"];
export function formatPlural({ locale, onError }, getPluralRules, value, options = {}) {
	if (!Intl.PluralRules) {
		onError(new FormatError(`Intl.PluralRules is not available in this environment.
Try polyfilling it using "@formatjs/intl-pluralrules"
`, ErrorCode.MISSING_INTL_API));
	}
	const filteredOptions = filterProps(options, PLURAL_FORMAT_OPTIONS);
	try {
		return getPluralRules(locale, filteredOptions).select(value);
	} catch (e) {
		onError(new IntlFormatError("Error formatting plural.", locale, e));
	}
	return "other";
}
