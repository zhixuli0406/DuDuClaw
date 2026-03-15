import "./types.js";
import { getNamedFormat, filterProps } from "./utils.js";
import { FormatError, ErrorCode } from "intl-messageformat";
import { IntlFormatError } from "./error.js";
const RELATIVE_TIME_FORMAT_OPTIONS = ["numeric", "style"];
function getFormatter({ locale, formats, onError }, getRelativeTimeFormat, options = {}) {
	const { format } = options;
	const defaults = !!format && getNamedFormat(formats, "relative", format, onError) || {};
	const filteredOptions = filterProps(options, RELATIVE_TIME_FORMAT_OPTIONS, defaults);
	return getRelativeTimeFormat(locale, filteredOptions);
}
export function formatRelativeTime(config, getRelativeTimeFormat, value, unit, options = {}) {
	if (!unit) {
		unit = "second";
	}
	const RelativeTimeFormat = Intl.RelativeTimeFormat;
	if (!RelativeTimeFormat) {
		config.onError(new FormatError(`Intl.RelativeTimeFormat is not available in this environment.
Try polyfilling it using "@formatjs/intl-relativetimeformat"
`, ErrorCode.MISSING_INTL_API));
	}
	try {
		return getFormatter(config, getRelativeTimeFormat, options).format(value, unit);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting relative time.", config.locale, e));
	}
	return String(value);
}
