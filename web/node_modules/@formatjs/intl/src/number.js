import "@formatjs/ecma402-abstract";
import { IntlFormatError } from "./error.js";
import "./types.js";
import { filterProps, getNamedFormat } from "./utils.js";
const NUMBER_FORMAT_OPTIONS = [
	"style",
	"currency",
	"unit",
	"unitDisplay",
	"useGrouping",
	"minimumIntegerDigits",
	"minimumFractionDigits",
	"maximumFractionDigits",
	"minimumSignificantDigits",
	"maximumSignificantDigits",
	"compactDisplay",
	"currencyDisplay",
	"currencySign",
	"notation",
	"signDisplay",
	"unit",
	"unitDisplay",
	"numberingSystem",
	"trailingZeroDisplay",
	"roundingPriority",
	"roundingIncrement",
	"roundingMode"
];
export function getFormatter({ locale, formats, onError }, getNumberFormat, options = {}) {
	const { format } = options;
	const defaults = format && getNamedFormat(formats, "number", format, onError) || {};
	const filteredOptions = filterProps(options, NUMBER_FORMAT_OPTIONS, defaults);
	return getNumberFormat(locale, filteredOptions);
}
export function formatNumber(config, getNumberFormat, value, options = {}) {
	try {
		return getFormatter(config, getNumberFormat, options).format(value);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting number.", config.locale, e));
	}
	return String(value);
}
export function formatNumberToParts(config, getNumberFormat, value, options = {}) {
	try {
		return getFormatter(config, getNumberFormat, options).formatToParts(value);
	} catch (e) {
		config.onError(new IntlFormatError("Error formatting number.", config.locale, e));
	}
	return [];
}
