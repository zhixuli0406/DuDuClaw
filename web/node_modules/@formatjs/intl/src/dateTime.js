import "./types.js";
import { IntlFormatError } from "./error.js";
import { filterProps, getNamedFormat } from "./utils.js";
const DATE_TIME_FORMAT_OPTIONS = [
	"formatMatcher",
	"timeZone",
	"hour12",
	"weekday",
	"era",
	"year",
	"month",
	"day",
	"hour",
	"minute",
	"second",
	"timeZoneName",
	"hourCycle",
	"dateStyle",
	"timeStyle",
	"calendar",
	"numberingSystem",
	"fractionalSecondDigits"
];
export function getFormatter({ locale, formats, onError, timeZone }, type, getDateTimeFormat, options = {}) {
	const { format } = options;
	const defaults = {
		...timeZone && { timeZone },
		...format && getNamedFormat(formats, type, format, onError)
	};
	let filteredOptions = filterProps(options, DATE_TIME_FORMAT_OPTIONS, defaults);
	if (type === "time" && !filteredOptions.hour && !filteredOptions.minute && !filteredOptions.second && !filteredOptions.timeStyle && !filteredOptions.dateStyle) {
		// Add default formatting options if hour, minute, or second isn't defined.
		filteredOptions = {
			...filteredOptions,
			hour: "numeric",
			minute: "numeric"
		};
	}
	return getDateTimeFormat(locale, filteredOptions);
}
export function formatDate(config, getDateTimeFormat, value, options = {}) {
	const date = typeof value === "string" ? new Date(value || 0) : value;
	try {
		return getFormatter(config, "date", getDateTimeFormat, options).format(date);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting date.", config.locale, e));
	}
	return String(date);
}
export function formatTime(config, getDateTimeFormat, value, options = {}) {
	const date = typeof value === "string" ? new Date(value || 0) : value;
	try {
		return getFormatter(config, "time", getDateTimeFormat, options).format(date);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting time.", config.locale, e));
	}
	return String(date);
}
export function formatDateTimeRange(config, getDateTimeFormat, from, to, options = {}) {
	const fromDate = typeof from === "string" ? new Date(from || 0) : from;
	const toDate = typeof to === "string" ? new Date(to || 0) : to;
	try {
		return getFormatter(config, "dateTimeRange", getDateTimeFormat, options).formatRange(fromDate, toDate);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting date time range.", config.locale, e));
	}
	return String(fromDate);
}
export function formatDateToParts(config, getDateTimeFormat, value, options = {}) {
	const date = typeof value === "string" ? new Date(value || 0) : value;
	try {
		return getFormatter(config, "date", getDateTimeFormat, options).formatToParts(date);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting date.", config.locale, e));
	}
	return [];
}
export function formatTimeToParts(config, getDateTimeFormat, value, options = {}) {
	const date = typeof value === "string" ? new Date(value || 0) : value;
	try {
		return getFormatter(config, "time", getDateTimeFormat, options).formatToParts(date);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting time.", config.locale, e));
	}
	return [];
}
